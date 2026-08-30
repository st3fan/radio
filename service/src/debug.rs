//! Always-on observability for the audio path (plan
//! 20260829-01-streaming-stall-debug): a heartbeat the player thread
//! updates around its blocking calls, a bounded ring of recent events,
//! and the stall check the monitor task runs off the player thread.
//!
//! Everything here is diagnostic state, deliberately outside `Status`:
//! `/status` and the UI are unchanged, and `/debug` makes no stability
//! promises. The stage is written *before* each blocking call, so an
//! observer can see where the player thread is stuck while it is stuck —
//! a thread wedged in a read cannot report on itself afterwards.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::sink::AudioSpec;

/// Bounded event history; at ~100 bytes an event this is a few KB, fixed.
const EVENT_CAPACITY: usize = 100;

/// No samples written for this long while the status says playing means
/// the audio has audibly stopped — the monitor logs the transition.
pub const STALL_THRESHOLD: Duration = Duration::from_secs(10);

/// Which blocking call (or wait) the player thread is in right now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Stage {
    /// Waiting for a command; nothing is supposed to play.
    Idle,
    /// Opening the source (network connect, probe, codec setup).
    Connecting,
    /// Blocked in `source.read()` — network for radio, bridge for AirPlay.
    Reading,
    /// Blocked in `sink.write()` — ALSA pacing, or a wedged device.
    Writing,
    /// Sleeping before the next reconnect attempt.
    Backoff,
}

impl Stage {
    pub fn name(self) -> &'static str {
        match self {
            Stage::Idle => "idle",
            Stage::Connecting => "connecting",
            Stage::Reading => "reading",
            Stage::Writing => "writing",
            Stage::Backoff => "backoff",
        }
    }
}

struct Event {
    at: SystemTime,
    kind: &'static str,
    detail: String,
}

struct Inner {
    stage: Stage,
    stage_entered: Instant,
    /// When the current playback session opened its sink; None while idle.
    session_started: Option<Instant>,
    spec: Option<AudioSpec>,
    /// Source-open attempts since boot (monotonic — climbing fast means
    /// the reconnect loop is failing).
    connect_attempts: u64,
    current_backoff: Option<Duration>,
    last_read: Option<Instant>,
    last_write: Option<Instant>,
    /// Samples this session; reset when a session opens.
    samples_read: u64,
    samples_written: u64,
    last_error: Option<(Instant, String)>,
    /// Set/cleared by the monitor's stall check.
    stalled_since: Option<Instant>,
    events: VecDeque<Event>,
}

/// Shared handle: the player thread writes, the control plane reads.
#[derive(Clone)]
pub struct DebugState(Arc<Mutex<Inner>>);

impl Default for DebugState {
    fn default() -> DebugState {
        DebugState::new()
    }
}

impl DebugState {
    pub fn new() -> DebugState {
        DebugState(Arc::new(Mutex::new(Inner {
            stage: Stage::Idle,
            stage_entered: Instant::now(),
            session_started: None,
            spec: None,
            connect_attempts: 0,
            current_backoff: None,
            last_read: None,
            last_write: None,
            samples_read: 0,
            samples_written: 0,
            last_error: None,
            stalled_since: None,
            events: VecDeque::new(),
        })))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.0.lock().expect("debug lock poisoned")
    }

    /// Set before every blocking call in the play loops.
    pub fn stage(&self, stage: Stage) {
        let mut inner = self.lock();
        if inner.stage != stage {
            inner.stage = stage;
            inner.stage_entered = Instant::now();
        }
        if stage == Stage::Idle {
            inner.session_started = None;
            inner.spec = None;
            inner.current_backoff = None;
        }
    }

    /// A source open is starting (stage becomes `connecting`).
    pub fn connect_attempt(&self, target: &str) {
        let mut inner = self.lock();
        inner.stage = Stage::Connecting;
        inner.stage_entered = Instant::now();
        inner.connect_attempts += 1;
        inner.current_backoff = None;
        push_event(&mut inner, "connect", target.to_string());
    }

    /// The sink opened: a playback session is live.
    pub fn session_open(&self, spec: AudioSpec) {
        let mut inner = self.lock();
        inner.session_started = Some(Instant::now());
        inner.spec = Some(spec);
        inner.samples_read = 0;
        inner.samples_written = 0;
        push_event(
            &mut inner,
            "session",
            format!("open: {} Hz, {} ch", spec.rate, spec.channels),
        );
    }

    pub fn read_ok(&self, samples: usize) {
        let mut inner = self.lock();
        inner.last_read = Some(Instant::now());
        inner.samples_read += samples as u64;
    }

    pub fn write_ok(&self, samples: usize) {
        let mut inner = self.lock();
        inner.last_write = Some(Instant::now());
        inner.samples_written += samples as u64;
    }

    /// A reconnect wait is starting (stage becomes `backoff`).
    pub fn backoff(&self, wait: Duration) {
        let mut inner = self.lock();
        inner.stage = Stage::Backoff;
        inner.stage_entered = Instant::now();
        inner.current_backoff = Some(wait);
        push_event(&mut inner, "backoff", format!("{:.1}s", wait.as_secs_f32()));
    }

    pub fn error(&self, message: &str) {
        let mut inner = self.lock();
        inner.last_error = Some((Instant::now(), message.to_string()));
        push_event(&mut inner, "error", message.to_string());
    }

    pub fn event(&self, kind: &'static str, detail: String) {
        push_event(&mut self.lock(), kind, detail);
    }

    /// The monitor's stall check, run off the player thread (which may be
    /// blocked). `playing` is the status the UI shows. Returns the
    /// transition, if any, so the caller can log it; the stalled flag and
    /// the stall/recovered events are recorded here.
    pub fn check_stall(&self, playing: bool, threshold: Duration) -> Option<StallTransition> {
        let mut inner = self.lock();
        // The reference point for "no audio for X": the last write, else
        // the session start, else (never opened — e.g. a reconnect loop
        // that never connects) the stage entry.
        let last_progress = inner
            .last_write
            .filter(|_| inner.session_started.is_some())
            .or(inner.session_started)
            .unwrap_or(inner.stage_entered);
        let stalled = playing && last_progress.elapsed() >= threshold;
        match (inner.stalled_since, stalled) {
            (None, true) => {
                inner.stalled_since = Some(Instant::now());
                let detail = format!(
                    "no audio written for {:.0}s; stage: {} (entered {:.0}s ago)",
                    last_progress.elapsed().as_secs_f32(),
                    inner.stage.name(),
                    inner.stage_entered.elapsed().as_secs_f32(),
                );
                push_event(&mut inner, "stall", detail.clone());
                Some(StallTransition::Stalled(detail))
            }
            (Some(since), false) => {
                inner.stalled_since = None;
                let detail = format!(
                    "audio flowing again after {:.0}s",
                    since.elapsed().as_secs_f32()
                );
                push_event(&mut inner, "recovered", detail.clone());
                Some(StallTransition::Recovered(detail))
            }
            _ => None,
        }
    }

    /// The `/debug` snapshot. `state` is the status's state string, passed
    /// in so this module needs no `Status` lock.
    pub fn snapshot(&self, state: &str) -> serde_json::Value {
        let inner = self.lock();
        let age_ms = |at: Option<Instant>| {
            at.map(|at| serde_json::json!(at.elapsed().as_millis() as u64))
                .unwrap_or(serde_json::Value::Null)
        };
        serde_json::json!({
            "state": state,
            "stage": inner.stage.name(),
            "stage_entered_ms_ago": inner.stage_entered.elapsed().as_millis() as u64,
            "session": inner.spec.map(|spec| serde_json::json!({
                "started_ms_ago": age_ms(inner.session_started),
                "rate": spec.rate,
                "channels": spec.channels,
                "samples_read": inner.samples_read,
                "samples_written": inner.samples_written,
            })),
            "last_read_ms_ago": age_ms(inner.last_read),
            "last_write_ms_ago": age_ms(inner.last_write),
            "connect_attempts": inner.connect_attempts,
            "current_backoff_ms": inner.current_backoff.map(|b| b.as_millis() as u64),
            "last_error": inner.last_error.as_ref().map(|(at, message)| serde_json::json!({
                "ms_ago": at.elapsed().as_millis() as u64,
                "message": message,
            })),
            "stalled_for_ms": age_ms(inner.stalled_since),
            "events": inner.events.iter().map(|event| serde_json::json!({
                "time": iso8601(event.at),
                "kind": event.kind,
                "detail": event.detail,
            })).collect::<Vec<_>>(),
        })
    }
}

pub enum StallTransition {
    Stalled(String),
    Recovered(String),
}

fn push_event(inner: &mut Inner, kind: &'static str, detail: String) {
    if inner.events.len() == EVENT_CAPACITY {
        inner.events.pop_front();
    }
    inner.events.push_back(Event {
        at: SystemTime::now(),
        kind,
        detail,
    });
}

/// UTC ISO 8601 from a SystemTime, seconds precision — enough to line
/// events up with the journal. Hand-rolled (civil-from-days) to avoid a
/// chrono dependency for one timestamp format.
fn iso8601(at: SystemTime) -> String {
    let secs = at
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let (hour, minute, second) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // Howard Hinnant's civil_from_days, shifted to the 0000-03-01 epoch.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> AudioSpec {
        AudioSpec {
            rate: 44100,
            channels: 2,
        }
    }

    #[test]
    fn snapshot_reports_stage_session_and_counters() {
        let debug = DebugState::new();
        let value = debug.snapshot("stopped");
        assert_eq!(value["stage"], "idle");
        assert_eq!(value["session"], serde_json::Value::Null);
        assert_eq!(value["last_write_ms_ago"], serde_json::Value::Null);

        debug.connect_attempt("https://example.com/stream");
        debug.session_open(spec());
        debug.stage(Stage::Reading);
        debug.read_ok(2048);
        debug.stage(Stage::Writing);
        debug.write_ok(2048);
        let value = debug.snapshot("playing");
        assert_eq!(value["state"], "playing");
        assert_eq!(value["stage"], "writing");
        assert_eq!(value["connect_attempts"], 1);
        assert_eq!(value["session"]["rate"], 44100);
        assert_eq!(value["session"]["samples_read"], 2048);
        assert_eq!(value["session"]["samples_written"], 2048);
        assert!(value["last_read_ms_ago"].is_u64());

        // Idle clears the session but keeps the history.
        debug.stage(Stage::Idle);
        let value = debug.snapshot("stopped");
        assert_eq!(value["stage"], "idle");
        assert_eq!(value["session"], serde_json::Value::Null);
        assert_eq!(value["connect_attempts"], 1, "monotonic across sessions");
    }

    #[test]
    fn a_new_session_resets_the_sample_counters() {
        let debug = DebugState::new();
        debug.session_open(spec());
        debug.read_ok(100);
        debug.write_ok(100);
        debug.session_open(spec());
        let value = debug.snapshot("playing");
        assert_eq!(value["session"]["samples_read"], 0);
        assert_eq!(value["session"]["samples_written"], 0);
    }

    #[test]
    fn event_ring_is_bounded() {
        let debug = DebugState::new();
        for i in 0..(EVENT_CAPACITY + 25) {
            debug.event("test", format!("event {i}"));
        }
        let value = debug.snapshot("stopped");
        let events = value["events"].as_array().unwrap();
        assert_eq!(events.len(), EVENT_CAPACITY);
        // Oldest dropped, newest kept.
        assert_eq!(events[0]["detail"], "event 25");
        assert_eq!(
            events[EVENT_CAPACITY - 1]["detail"],
            format!("event {}", EVENT_CAPACITY + 24)
        );
    }

    #[test]
    fn errors_and_backoff_reach_the_snapshot() {
        let debug = DebugState::new();
        debug.error("connection refused");
        debug.backoff(Duration::from_millis(500));
        let value = debug.snapshot("playing");
        assert_eq!(value["stage"], "backoff");
        assert_eq!(value["current_backoff_ms"], 500);
        assert_eq!(value["last_error"]["message"], "connection refused");
        let events = value["events"].as_array().unwrap();
        assert_eq!(events[0]["kind"], "error");
        assert_eq!(events[1]["kind"], "backoff");
    }

    #[test]
    fn stall_check_detects_and_recovers() {
        let debug = DebugState::new();
        debug.session_open(spec());
        debug.write_ok(2048);

        // Fresh write: not stalled.
        assert!(debug.check_stall(true, Duration::from_millis(50)).is_none());
        std::thread::sleep(Duration::from_millis(60));
        // Old write while playing: one Stalled transition, then quiet.
        assert!(matches!(
            debug.check_stall(true, Duration::from_millis(50)),
            Some(StallTransition::Stalled(_))
        ));
        assert!(
            debug.check_stall(true, Duration::from_millis(50)).is_none(),
            "no repeat while still stalled"
        );
        let value = debug.snapshot("playing");
        assert!(value["stalled_for_ms"].is_u64());

        // Audio flows again: one Recovered transition.
        debug.write_ok(2048);
        assert!(matches!(
            debug.check_stall(true, Duration::from_millis(50)),
            Some(StallTransition::Recovered(_))
        ));
        assert_eq!(
            debug.snapshot("playing")["stalled_for_ms"],
            serde_json::Value::Null
        );
    }

    #[test]
    fn stall_check_ignores_not_playing() {
        let debug = DebugState::new();
        // Idle forever is not a stall — nothing is supposed to play.
        std::thread::sleep(Duration::from_millis(10));
        assert!(debug.check_stall(false, Duration::from_millis(5)).is_none());
    }

    #[test]
    fn stall_check_catches_a_session_that_never_wrote() {
        // The blocked-first-read case: the session opened but no write
        // ever happened; the session start is the reference point.
        let debug = DebugState::new();
        debug.session_open(spec());
        debug.stage(Stage::Reading);
        std::thread::sleep(Duration::from_millis(20));
        let transition = debug.check_stall(true, Duration::from_millis(10));
        let Some(StallTransition::Stalled(detail)) = transition else {
            panic!("expected a stall");
        };
        assert!(detail.contains("stage: reading"), "detail: {detail}");
    }

    #[test]
    fn iso8601_formats_known_timestamps() {
        let at = UNIX_EPOCH + Duration::from_secs(0);
        assert_eq!(iso8601(at), "1970-01-01T00:00:00Z");
        // 2026-08-30 17:53:15 UTC.
        let at = UNIX_EPOCH + Duration::from_secs(1_788_112_395);
        assert_eq!(iso8601(at), "2026-08-30T17:53:15Z");
    }
}
