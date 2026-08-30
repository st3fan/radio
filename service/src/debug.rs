//! Always-on observability for the audio pipeline: a heartbeat the player
//! thread updates at the natural points of its loop, a bounded ring of
//! recent events, and a stall monitor that watches both from the control
//! plane. Deliberately separate from `Status` — the `/status` contract
//! stays untouched, and this shape is diagnostic, not API.
//!
//! The design premise: when the radio goes silent while the UI says
//! `playing`, the player thread itself may be blocked and unable to
//! answer. So the stage is written *before* every blocking call (an
//! observer sees where the thread is stuck while it is stuck), and stall
//! detection runs on the control plane, never on the thread being
//! diagnosed. See `plans/20260829-01-streaming-stall-debug.md`.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::status::{State, Status};

/// Fixed bound on the event ring: a few kilobytes, enough to cover "what
/// happened at 3 AM" without ever growing.
const EVENT_CAPACITY: usize = 100;

/// No audio written for this long while `playing` counts as a stall.
pub const STALL_THRESHOLD: Duration = Duration::from_secs(10);

/// How often the monitor samples the heartbeat.
const MONITOR_INTERVAL: Duration = Duration::from_secs(2);

/// Which blocking call the play loop is in (or about to enter). Written
/// before the call, so it stays accurate while the call never returns.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Stage {
    Idle,
    Connecting,
    Reading,
    Writing,
    Backoff,
    Airplay,
}

impl Stage {
    pub fn name(self) -> &'static str {
        match self {
            Stage::Idle => "idle",
            Stage::Connecting => "connecting",
            Stage::Reading => "reading",
            Stage::Writing => "writing",
            Stage::Backoff => "backoff",
            Stage::Airplay => "airplay",
        }
    }
}

/// The heartbeat. Timestamps are monotonic `Instant`s; they leave the
/// process only as ages (`…_ms_ago`), which sidesteps clock questions.
struct Health {
    stage: Stage,
    stage_entered_at: Instant,
    /// When the current session (one station across its reconnects, or
    /// one AirPlay stream) started; the attempt/sample counters below
    /// reset with it.
    session_started_at: Option<Instant>,
    stream_url: Option<String>,
    connect_attempts: u32,
    current_backoff: Option<Duration>,
    last_read_at: Option<Instant>,
    last_write_at: Option<Instant>,
    samples_read: u64,
    samples_written: u64,
    last_error: Option<(String, Instant)>,
    /// Owned by the stall monitor; surfaces in the snapshot.
    stalled: bool,
    stalled_since: Option<Instant>,
}

struct Event {
    at: SystemTime,
    kind: &'static str,
    detail: String,
}

struct Inner {
    health: Health,
    events: VecDeque<Event>,
}

impl Inner {
    fn push(&mut self, kind: &'static str, detail: String) {
        if self.events.len() == EVENT_CAPACITY {
            self.events.pop_front();
        }
        self.events.push_back(Event {
            at: SystemTime::now(),
            kind,
            detail,
        });
    }

    /// A session reset: counters and errors start over, the stall flag
    /// stays (the monitor clears it once audio actually flows again).
    fn start_session(&mut self, stream_url: Option<String>) {
        let health = &mut self.health;
        health.session_started_at = Some(Instant::now());
        health.stream_url = stream_url;
        health.connect_attempts = 0;
        health.current_backoff = None;
        health.last_read_at = None;
        health.last_write_at = None;
        health.samples_read = 0;
        health.samples_written = 0;
        health.last_error = None;
    }
}

/// The shared debug state: written by the player thread, read by the
/// monitor task and (later) the `/debug` endpoint.
pub struct DebugState {
    inner: Mutex<Inner>,
}

impl Default for DebugState {
    fn default() -> DebugState {
        DebugState::new()
    }
}

impl DebugState {
    pub fn new() -> DebugState {
        DebugState {
            inner: Mutex::new(Inner {
                health: Health {
                    stage: Stage::Idle,
                    stage_entered_at: Instant::now(),
                    session_started_at: None,
                    stream_url: None,
                    connect_attempts: 0,
                    current_backoff: None,
                    last_read_at: None,
                    last_write_at: None,
                    samples_read: 0,
                    samples_written: 0,
                    last_error: None,
                    stalled: false,
                    stalled_since: None,
                },
                events: VecDeque::with_capacity(EVENT_CAPACITY),
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().expect("debug lock poisoned")
    }

    /// Records which blocking call comes next. Re-entering the current
    /// stage keeps `stage_entered_at`, so "in this stage since" stays
    /// honest for stages held across iterations.
    pub fn set_stage(&self, stage: Stage) {
        let mut inner = self.lock();
        let health = &mut inner.health;
        if health.stage != stage {
            health.stage = stage;
            health.stage_entered_at = Instant::now();
            if stage != Stage::Backoff {
                health.current_backoff = None;
            }
        }
    }

    pub fn radio_session_started(&self, stream_url: &str) {
        let mut inner = self.lock();
        inner.start_session(Some(stream_url.to_string()));
        inner.push("session", format!("radio: {stream_url}"));
    }

    pub fn airplay_session_started(&self, rate: u32, channels: u16) {
        let mut inner = self.lock();
        inner.start_session(None);
        inner.health.stage = Stage::Airplay;
        inner.health.stage_entered_at = Instant::now();
        inner.push("session", format!("airplay: {rate} Hz, {channels} ch"));
    }

    pub fn connect_started(&self, url: &str) {
        let mut inner = self.lock();
        let health = &mut inner.health;
        if health.stage != Stage::Connecting {
            health.stage = Stage::Connecting;
            health.stage_entered_at = Instant::now();
        }
        health.current_backoff = None;
        health.connect_attempts += 1;
        let attempt = health.connect_attempts;
        inner.push("connect", format!("attempt {attempt}: {url}"));
    }

    pub fn connected(&self, url: &str) {
        self.lock().push("connected", url.to_string());
    }

    pub fn connect_failed(&self, error: &str) {
        let mut inner = self.lock();
        inner.health.last_error = Some((error.to_string(), Instant::now()));
        inner.push("connect_failed", error.to_string());
    }

    pub fn backoff(&self, delay: Duration) {
        let mut inner = self.lock();
        let health = &mut inner.health;
        health.stage = Stage::Backoff;
        health.stage_entered_at = Instant::now();
        health.current_backoff = Some(delay);
        inner.push("backoff", format!("{:.1}s", delay.as_secs_f32()));
    }

    pub fn note_read(&self, samples: usize) {
        let health = &mut self.lock().health;
        health.last_read_at = Some(Instant::now());
        health.samples_read += samples as u64;
    }

    pub fn note_write(&self, samples: usize) {
        let health = &mut self.lock().health;
        health.last_write_at = Some(Instant::now());
        health.samples_written += samples as u64;
    }

    pub fn source_eof(&self) {
        self.lock().push("eof", String::new());
    }

    pub fn read_error(&self, error: &str) {
        let mut inner = self.lock();
        inner.health.last_error = Some((error.to_string(), Instant::now()));
        inner.push("read_error", error.to_string());
    }

    pub fn sink_error(&self, error: &str) {
        let mut inner = self.lock();
        inner.health.last_error = Some((error.to_string(), Instant::now()));
        inner.push("sink_error", error.to_string());
    }

    pub fn command(&self, name: &'static str) {
        self.lock().push("command", name.to_string());
    }

    pub fn event(&self, kind: &'static str, detail: String) {
        self.lock().push(kind, detail);
    }

    pub fn snapshot(&self) -> Snapshot {
        let inner = self.lock();
        let health = &inner.health;
        let ms_ago = |at: Instant| at.elapsed().as_millis() as u64;
        Snapshot {
            stage: health.stage,
            stage_ms_ago: ms_ago(health.stage_entered_at),
            session_started_ms_ago: health.session_started_at.map(ms_ago),
            stream_url: health.stream_url.clone(),
            connect_attempts: health.connect_attempts,
            current_backoff_ms: health.current_backoff.map(|delay| delay.as_millis() as u64),
            last_read_ms_ago: health.last_read_at.map(ms_ago),
            last_write_ms_ago: health.last_write_at.map(ms_ago),
            samples_read: health.samples_read,
            samples_written: health.samples_written,
            last_error: health.last_error.as_ref().map(|(message, at)| ErrorInfo {
                message: message.clone(),
                ms_ago: ms_ago(*at),
            }),
            stalled: health.stalled,
            events: inner
                .events
                .iter()
                .rev() // newest first: the interesting end when reading from the couch
                .map(|event| EventInfo {
                    unix_ms: event
                        .at
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64,
                    kind: event.kind,
                    detail: event.detail.clone(),
                })
                .collect(),
        }
    }
}

/// A point-in-time copy of the heartbeat and events, ages in ms. This is
/// what `/debug` serializes; the shape is explicitly not a stable API.
#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub stage: Stage,
    pub stage_ms_ago: u64,
    pub session_started_ms_ago: Option<u64>,
    pub stream_url: Option<String>,
    pub connect_attempts: u32,
    pub current_backoff_ms: Option<u64>,
    pub last_read_ms_ago: Option<u64>,
    pub last_write_ms_ago: Option<u64>,
    pub samples_read: u64,
    pub samples_written: u64,
    pub last_error: Option<ErrorInfo>,
    pub stalled: bool,
    /// Newest first.
    pub events: Vec<EventInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ErrorInfo {
    pub message: String,
    pub ms_ago: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct EventInfo {
    pub unix_ms: u64,
    pub kind: &'static str,
    pub detail: String,
}

/// A stall state change, for the monitor's journal lines.
#[derive(Debug)]
pub enum StallTransition {
    Entered {
        stage: Stage,
        no_audio_for: Duration,
        stage_for: Duration,
    },
    Ended {
        after: Duration,
    },
}

/// One monitor pass: stalled means `playing` while no audio has been
/// written for `threshold` (measured from the last write, or the session
/// start when nothing was ever written). Returns the transition when the
/// stall state flips — once per stall, not per pass.
pub fn check_stall(
    status: &Mutex<Status>,
    debug: &DebugState,
    threshold: Duration,
) -> Option<StallTransition> {
    let playing = status.lock().expect("status lock poisoned").state == State::Playing;
    let mut inner = debug.lock();
    let health = &inner.health;
    let reference = health.last_write_at.or(health.session_started_at);
    let stalled = playing && reference.is_some_and(|at| at.elapsed() >= threshold);
    if stalled == health.stalled {
        return None;
    }
    if stalled {
        let stage = health.stage;
        let no_audio_for = reference.map(|at| at.elapsed()).unwrap_or_default();
        let stage_for = health.stage_entered_at.elapsed();
        inner.health.stalled = true;
        inner.health.stalled_since = Some(Instant::now());
        inner.push(
            "stall",
            format!(
                "no audio for {:.1}s (stage: {})",
                no_audio_for.as_secs_f32(),
                stage.name()
            ),
        );
        Some(StallTransition::Entered {
            stage,
            no_audio_for,
            stage_for,
        })
    } else {
        inner.health.stalled = false;
        let after = inner
            .health
            .stalled_since
            .take()
            .map(|at| at.elapsed())
            .unwrap_or_default();
        inner.push("recovered", format!("after {:.1}s", after.as_secs_f32()));
        Some(StallTransition::Ended { after })
    }
}

/// The stall monitor: a control-plane task (like the state saver), so it
/// keeps observing when the player thread is wedged. Logs one line on
/// each transition — timestamped evidence in the journal even when
/// nobody is watching.
pub fn spawn_monitor(status: Arc<Mutex<Status>>, debug: Arc<DebugState>) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(MONITOR_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            match check_stall(&status, &debug, STALL_THRESHOLD) {
                Some(StallTransition::Entered {
                    stage,
                    no_audio_for,
                    stage_for,
                }) => eprintln!(
                    "radiod: stall: playing but no audio written for {:.1}s \
                     (stage: {} for {:.1}s)",
                    no_audio_for.as_secs_f32(),
                    stage.name(),
                    stage_for.as_secs_f32()
                ),
                Some(StallTransition::Ended { after }) => {
                    eprintln!("radiod: stall: ended after {:.1}s", after.as_secs_f32());
                }
                None => {}
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn playing_status() -> Mutex<Status> {
        let mut status = Status::initial(&Config::default());
        status.state = State::Playing;
        Mutex::new(status)
    }

    #[test]
    fn event_ring_is_bounded_and_newest_first() {
        let debug = DebugState::new();
        for i in 0..150 {
            debug.event("test", format!("event {i}"));
        }
        let snapshot = debug.snapshot();
        assert_eq!(snapshot.events.len(), EVENT_CAPACITY);
        assert_eq!(snapshot.events[0].detail, "event 149");
        assert_eq!(snapshot.events[99].detail, "event 50");
    }

    #[test]
    fn reentering_a_stage_keeps_its_entry_time() {
        let debug = DebugState::new();
        debug.set_stage(Stage::Reading);
        std::thread::sleep(Duration::from_millis(10));
        debug.set_stage(Stage::Reading);
        assert!(debug.snapshot().stage_ms_ago >= 10);
        debug.set_stage(Stage::Writing);
        assert!(debug.snapshot().stage_ms_ago < 10);
    }

    #[test]
    fn counters_and_ages_track_reads_and_writes() {
        let debug = DebugState::new();
        debug.radio_session_started("https://example.com/stream");
        debug.note_read(100);
        debug.note_read(100);
        debug.note_write(80);
        let snapshot = debug.snapshot();
        assert_eq!(snapshot.samples_read, 200);
        assert_eq!(snapshot.samples_written, 80);
        assert!(snapshot.last_read_ms_ago.unwrap() < 1000);
        assert!(snapshot.last_write_ms_ago.unwrap() < 1000);
        assert_eq!(
            snapshot.stream_url.as_deref(),
            Some("https://example.com/stream")
        );
    }

    #[test]
    fn a_new_session_resets_counters_but_not_the_stall_flag() {
        let status = playing_status();
        let debug = DebugState::new();
        debug.radio_session_started("https://example.com/one");
        debug.connect_started("https://example.com/one");
        debug.note_read(100);
        debug.note_write(100);
        assert!(matches!(
            check_stall(&status, &debug, Duration::ZERO),
            Some(StallTransition::Entered { .. })
        ));
        debug.radio_session_started("https://example.com/two");
        let snapshot = debug.snapshot();
        assert_eq!(snapshot.samples_read, 0);
        assert_eq!(snapshot.connect_attempts, 0);
        assert!(snapshot.last_error.is_none());
        assert!(snapshot.stalled, "the monitor owns clearing the flag");
    }

    #[test]
    fn stall_transitions_fire_once_each_way() {
        let status = playing_status();
        let debug = DebugState::new();

        // No session yet: playing but nothing to measure against.
        assert!(check_stall(&status, &debug, Duration::ZERO).is_none());

        debug.radio_session_started("https://example.com/stream");
        let entered = check_stall(&status, &debug, Duration::ZERO);
        assert!(matches!(
            entered,
            Some(StallTransition::Entered {
                stage: Stage::Idle,
                ..
            })
        ));
        // Still stalled: no second transition.
        assert!(check_stall(&status, &debug, Duration::ZERO).is_none());
        assert!(debug.snapshot().stalled);

        // Audio flows again: one Ended transition, then quiet.
        debug.note_write(2048);
        assert!(matches!(
            check_stall(&status, &debug, Duration::from_secs(10)),
            Some(StallTransition::Ended { .. })
        ));
        assert!(check_stall(&status, &debug, Duration::from_secs(10)).is_none());
        assert!(!debug.snapshot().stalled);

        // The transitions left their events, newest first.
        let events = debug.snapshot().events;
        assert_eq!(events[0].kind, "recovered");
        assert_eq!(events[1].kind, "stall");
    }

    #[test]
    fn a_stopped_player_is_never_stalled() {
        let status = Mutex::new(Status::initial(&Config::default()));
        let debug = DebugState::new();
        debug.radio_session_started("https://example.com/stream");
        assert!(check_stall(&status, &debug, Duration::ZERO).is_none());
        assert!(!debug.snapshot().stalled);
    }

    #[test]
    fn stopping_mid_stall_ends_the_stall() {
        let status = playing_status();
        let debug = DebugState::new();
        debug.radio_session_started("https://example.com/stream");
        assert!(matches!(
            check_stall(&status, &debug, Duration::ZERO),
            Some(StallTransition::Entered { .. })
        ));
        status.lock().unwrap().state = State::Stopped;
        assert!(matches!(
            check_stall(&status, &debug, Duration::ZERO),
            Some(StallTransition::Ended { .. })
        ));
    }

    #[test]
    fn snapshot_serializes_with_lowercase_stage() {
        let debug = DebugState::new();
        debug.radio_session_started("https://example.com/stream");
        debug.set_stage(Stage::Reading);
        debug.connect_failed("boom");
        let value = serde_json::to_value(debug.snapshot()).unwrap();
        assert_eq!(value["stage"], "reading");
        assert_eq!(value["stalled"], false);
        assert_eq!(value["last_error"]["message"], "boom");
        assert_eq!(value["events"][0]["kind"], "connect_failed");
        assert!(value["events"][0]["unix_ms"].as_u64().unwrap() > 0);
    }
}
