//! The player thread: owns the audio pipeline and is the only writer of
//! playback state transitions in the shared status.

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::airplay::AirplaySource;
use crate::debug::{DebugState, Stage};
use crate::mixer::MixerControl;
use crate::sink::AudioSink;
use crate::source::{Source, SourceFactory};
use crate::status::{AirplayInfo, AudioSource, State, Status};
use crate::volume;

/// The optional hardware-ceiling owner; None for the dev sinks.
type Mixer = Option<Box<dyn MixerControl>>;

/// The two observation planes the player writes to: the public status
/// (the API contract) and the always-on debug heartbeat (diagnostics).
struct Shared<'a> {
    status: &'a Mutex<Status>,
    debug: &'a DebugState,
}

const CHUNK_SAMPLES: usize = 2048;

/// Reconnect behavior. Injectable so tests run in milliseconds.
#[derive(Debug, Clone, Copy)]
pub struct Tuning {
    /// First wait before reconnecting after a dropped stream.
    pub initial_backoff: Duration,
    /// Backoff doubles up to this ceiling.
    pub max_backoff: Duration,
    /// A session that played at least this long resets the backoff.
    pub stable_after: Duration,
}

impl Default for Tuning {
    fn default() -> Tuning {
        Tuning {
            initial_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(30),
            stable_after: Duration::from_secs(30),
        }
    }
}

#[derive(Debug)]
pub enum Command {
    Play {
        playlist_url: String,
        stream_url: String,
    },
    Stop,
    Pause,
    Resume,
    /// An AirPlay stream negotiated its sink: the session takes over the
    /// pipeline. Sent by the bridge's sink factory.
    AirplayStarted {
        source: AirplaySource,
    },
    /// The AirPlay session is over (TEARDOWN / connection closed).
    AirplayEnded,
}

/// What is (or was) playing. Kept across pause so resume can reconnect.
#[derive(Debug, Clone)]
struct Station {
    playlist_url: String,
    stream_url: String,
}

enum Session {
    Idle,
    Playing(Station),
    Paused(Station),
    /// An AirPlay sender owns the pipeline; the preempted station (if any)
    /// is remembered so the radio can come back afterwards.
    Airplay {
        source: AirplaySource,
        remembered: Option<Station>,
    },
}

/// Handle used by the HTTP server to control the player thread.
#[derive(Clone)]
pub struct Player {
    tx: Sender<Command>,
}

impl Player {
    pub fn send(&self, command: Command) {
        // The player thread lives for the process lifetime; a send can only
        // fail if it panicked, which is already fatal for the daemon.
        if self.tx.send(command).is_err() {
            eprintln!("radiod: player thread is gone");
        }
    }
}

/// Test convenience: no mixer, default tuning, resume-radio on.
/// Production goes through `spawn_with_tuning` so the mixer is threaded in.
#[cfg(test)]
pub fn spawn(
    status: Arc<Mutex<Status>>,
    sink: Box<dyn AudioSink>,
    source_factory: SourceFactory,
) -> Player {
    spawn_with_tuning(
        status,
        sink,
        None,
        source_factory,
        Tuning::default(),
        true,
        Arc::new(DebugState::new()),
    )
    .0
}

/// Also returns the thread's join handle: the thread exits (closing the
/// sink) when every `Player` handle is dropped. `resume_radio` is the
/// end-of-AirPlay-session policy.
pub fn spawn_with_tuning(
    status: Arc<Mutex<Status>>,
    sink: Box<dyn AudioSink>,
    mixer: Mixer,
    source_factory: SourceFactory,
    tuning: Tuning,
    resume_radio: bool,
    debug: Arc<DebugState>,
) -> (Player, std::thread::JoinHandle<()>) {
    let (tx, rx) = channel();
    let handle = std::thread::spawn(move || {
        let shared = Shared {
            status: &status,
            debug: &debug,
        };
        run(
            &rx,
            &shared,
            sink,
            mixer,
            &source_factory,
            tuning,
            resume_radio,
        )
    });
    (Player { tx }, handle)
}

fn run(
    rx: &Receiver<Command>,
    shared: &Shared,
    mut sink: Box<dyn AudioSink>,
    mut mixer: Mixer,
    source_factory: &SourceFactory,
    tuning: Tuning,
    resume_radio: bool,
) {
    let mut session = Session::Idle;
    loop {
        session = match session {
            Session::Playing(station) => {
                match play_with_retries(
                    rx,
                    shared,
                    sink.as_mut(),
                    &mut mixer,
                    source_factory,
                    &station,
                    tuning,
                ) {
                    RetryEnd::Command(command) => {
                        transition(command, Session::Playing(station), shared, resume_radio)
                    }
                    RetryEnd::Fatal => {
                        set_stopped(shared.status);
                        Session::Idle
                    }
                    RetryEnd::Disconnected => return,
                }
            }
            Session::Airplay {
                mut source,
                remembered,
            } => match play_airplay(rx, shared, sink.as_mut(), &mut mixer, &mut source) {
                AirplayEnd::Command(command) => transition(
                    command,
                    Session::Airplay { source, remembered },
                    shared,
                    resume_radio,
                ),
                AirplayEnd::Ended => end_airplay(remembered, shared.status, resume_radio),
                AirplayEnd::Fatal => {
                    set_stopped(shared.status);
                    Session::Idle
                }
                AirplayEnd::Disconnected => return,
            },
            idle_or_paused => {
                shared.debug.set_stage(Stage::Idle);
                let Ok(command) = rx.recv() else { return };
                transition(command, idle_or_paused, shared, resume_radio)
            }
        };
    }
}

enum RetryEnd {
    Command(Command),
    Fatal,
    Disconnected,
}

/// Plays a station, reconnecting with backoff whenever the source drops.
/// While this loop runs, the status stays `playing` — a radio should ride
/// out network blips by itself; the user's remedy is /stop. Only sink
/// failures (the audio device, not the network) are fatal.
fn play_with_retries(
    rx: &Receiver<Command>,
    shared: &Shared,
    sink: &mut dyn AudioSink,
    mixer: &mut Mixer,
    source_factory: &SourceFactory,
    station: &Station,
    tuning: Tuning,
) -> RetryEnd {
    shared.debug.radio_session_started(&station.stream_url);
    let mut backoff = tuning.initial_backoff;
    loop {
        let started = Instant::now();
        match play(rx, shared, sink, mixer, source_factory, station) {
            Outcome::Interrupted(command) => return RetryEnd::Command(command),
            Outcome::Disconnected => return RetryEnd::Disconnected,
            Outcome::SinkFailed => return RetryEnd::Fatal,
            Outcome::SourceEnded => {
                if started.elapsed() >= tuning.stable_after {
                    backoff = tuning.initial_backoff;
                }
                eprintln!(
                    "radiod: reconnecting to {} in {:.1}s",
                    station.stream_url,
                    backoff.as_secs_f32()
                );
                shared.debug.backoff(backoff);
                // recv_timeout keeps the wait responsive: /stop, /pause,
                // and /play interrupt a backoff sleep immediately.
                match rx.recv_timeout(backoff) {
                    Ok(command) => return RetryEnd::Command(command),
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => return RetryEnd::Disconnected,
                }
                backoff = (backoff * 2).min(tuning.max_backoff);
            }
        }
    }
}

/// Applies a command to the current session. The only writer of pause/stop
/// status transitions; `play()` itself sets `playing`.
fn transition(command: Command, session: Session, shared: &Shared, resume_radio: bool) -> Session {
    let status = shared.status;
    shared.debug.command(match &command {
        Command::Play { .. } => "play",
        Command::Stop => "stop",
        Command::Pause => "pause",
        Command::Resume => "resume",
        Command::AirplayStarted { .. } => "airplay_started",
        Command::AirplayEnded => "airplay_ended",
    });
    match command {
        // The server refuses /play during an AirPlay session (409); if a
        // Play races through anyway, the station wins and the abandoned
        // bridge just drops the sender's audio.
        Command::Play {
            playlist_url,
            stream_url,
        } => Session::Playing(Station {
            playlist_url,
            stream_url,
        }),
        Command::Stop => {
            set_stopped(status);
            Session::Idle
        }
        Command::Pause => match session {
            // Keep the station so resume can reconnect; the URLs stay in
            // the status too.
            Session::Playing(station) | Session::Paused(station) => {
                set_paused(status);
                Session::Paused(station)
            }
            // Transport control belongs to the sender during AirPlay; the
            // server refuses these, so arriving here is a no-op.
            airplay @ Session::Airplay { .. } => airplay,
            Session::Idle => Session::Idle,
        },
        Command::Resume => match session {
            // From Playing this only happens when a Resume interrupted the
            // play loop; the pipeline was torn down, so just restart it.
            Session::Playing(station) | Session::Paused(station) => Session::Playing(station),
            airplay @ Session::Airplay { .. } => airplay,
            Session::Idle => Session::Idle,
        },
        Command::AirplayStarted { source } => {
            // Preempt whatever plays and remember it; a new AirPlay stream
            // (e.g. the sender re-negotiated) keeps the earlier memory.
            let remembered = match session {
                Session::Playing(station) | Session::Paused(station) => Some(station),
                Session::Airplay { remembered, .. } => remembered,
                Session::Idle => None,
            };
            Session::Airplay { source, remembered }
        }
        Command::AirplayEnded => match session {
            Session::Airplay { remembered, .. } => end_airplay(remembered, status, resume_radio),
            // The session already ended through the bridge (EOF); ignore.
            other => other,
        },
    }
}

/// End-of-AirPlay policy: back to the remembered station, or idle.
fn end_airplay(remembered: Option<Station>, status: &Mutex<Status>, resume_radio: bool) -> Session {
    {
        let mut status = status.lock().expect("status lock poisoned");
        status.source = AudioSource::Radio;
        status.airplay = None;
        status.airplay_gain = 1.0;
        // The library sends no clear events at teardown — dropping the
        // sender's now-playing state is our job.
        status.airplay_track = None;
        status.airplay_artwork = None;
    }
    match remembered {
        Some(station) if resume_radio => Session::Playing(station),
        _ => {
            set_stopped(status);
            Session::Idle
        }
    }
}

enum Outcome {
    Interrupted(Command),
    /// Source open failure, EOF, or read error — retryable.
    SourceEnded,
    /// The audio device failed — fatal.
    SinkFailed,
    Disconnected,
}

/// Plays one station session until stopped, interrupted by a new command,
/// or the source ends.
fn play(
    rx: &Receiver<Command>,
    shared: &Shared,
    sink: &mut dyn AudioSink,
    mixer: &mut Mixer,
    source_factory: &SourceFactory,
    station: &Station,
) -> Outcome {
    let status = shared.status;
    // The hardware ceiling comes first: every session start (including
    // each reconnect attempt, which covers the USB-DAC-reappeared case)
    // re-asserts it, and a session that cannot get its ceiling does not
    // get audio. Fatal like a sink failure — the operator must intervene.
    if let Some(mixer) = mixer.as_mut() {
        match mixer.assert_ceiling() {
            Ok(()) => {
                let mut status = status.lock().expect("status lock poisoned");
                if status.mixer != "ok" {
                    status.mixer = "ok".to_string();
                }
            }
            Err(err) => {
                eprintln!("radiod: mixer: {err}");
                status.lock().expect("status lock poisoned").mixer = format!("error: {err}");
                return Outcome::SinkFailed;
            }
        }
    }

    // Optimistic: `playing` means "trying to play". Set before opening so
    // the status holds steady through reconnect attempts. The song title
    // resets per session (live radio moved on), but the station name is
    // kept when the station is unchanged — resume and reconnects must not
    // blank it; only an actual station switch clears it.
    {
        let mut status = status.lock().expect("status lock poisoned");
        let same_station = status.playlist_url.as_deref() == Some(station.playlist_url.as_str());
        status.state = State::Playing;
        status.source = AudioSource::Radio;
        status.airplay = None;
        status.airplay_track = None;
        status.airplay_artwork = None;
        status.playlist_url = Some(station.playlist_url.clone());
        status.stream_url = Some(station.stream_url.clone());
        status.icy_title = None;
        if !same_station {
            status.icy_name = None;
        }
    }

    let stream_url = station.stream_url.as_str();
    shared.debug.connect_started(stream_url);
    let mut source = match source_factory(stream_url) {
        Ok(source) => source,
        Err(err) => {
            eprintln!("radiod: cannot open {stream_url}: {err}");
            shared.debug.connect_failed(&err.to_string());
            return Outcome::SourceEnded;
        }
    };
    shared.debug.connected(stream_url);
    if let Err(err) = sink.open(source.spec()) {
        eprintln!("radiod: cannot open audio sink: {err}");
        shared.debug.sink_error(&err.to_string());
        return Outcome::SinkFailed;
    }

    let mut buf = vec![0i16; CHUNK_SAMPLES];
    let outcome = loop {
        match rx.try_recv() {
            Ok(command) => break Outcome::Interrupted(command),
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => break Outcome::Disconnected,
        }

        // The stage is set *before* each blocking call so /debug shows
        // where the loop is stuck even when the call never returns.
        shared.debug.set_stage(Stage::Reading);
        let n = match source.read(&mut buf) {
            Ok(0) => {
                eprintln!("radiod: stream ended: {stream_url}");
                shared.debug.source_eof();
                break Outcome::SourceEnded;
            }
            Ok(n) => n,
            Err(err) => {
                // A timeout is the read watchdog firing on a stalled
                // connection — the very case this exists to catch;
                // record it distinctly, then reconnect like any drop.
                if err.timed_out() {
                    eprintln!("radiod: read timed out on {stream_url}: {err}");
                    shared.debug.read_timeout(&err.to_string());
                } else {
                    eprintln!("radiod: error reading {stream_url}: {err}");
                    shared.debug.read_error(&err.to_string());
                }
                break Outcome::SourceEnded;
            }
        };
        shared.debug.note_read(n);

        if let Some(icy) = source.icy() {
            let mut status = status.lock().expect("status lock poisoned");
            status.icy_title = icy.title;
            status.icy_name = icy.name;
        }

        let gain = {
            let status = status.lock().expect("status lock poisoned");
            volume::gain(status.volume, status.muted)
        };
        volume::apply_gain(&mut buf[..n], gain);

        shared.debug.set_stage(Stage::Writing);
        if let Err(err) = sink.write(&buf[..n]) {
            eprintln!("radiod: error writing to audio sink: {err}");
            shared.debug.sink_error(&err.to_string());
            break Outcome::SinkFailed;
        }
        shared.debug.note_write(n);
    };

    sink.close();
    outcome
}

enum AirplayEnd {
    Command(Command),
    /// The bridge reported end of stream: the library dropped its sink.
    Ended,
    Fatal,
    Disconnected,
}

/// Plays one AirPlay session until it ends, a command interrupts, or the
/// sink fails. Unlike radio, there is no reconnect loop — the sender owns
/// the session's lifetime.
fn play_airplay(
    rx: &Receiver<Command>,
    shared: &Shared,
    sink: &mut dyn AudioSink,
    mixer: &mut Mixer,
    source: &mut AirplaySource,
) -> AirplayEnd {
    let status = shared.status;
    // Same ceiling-first rule as the radio path: no verified ceiling, no
    // audio — an AirPlay sender at slider-max must meet the same hardware
    // cap as everything else.
    if let Some(mixer) = mixer.as_mut() {
        match mixer.assert_ceiling() {
            Ok(()) => {
                let mut status = status.lock().expect("status lock poisoned");
                if status.mixer != "ok" {
                    status.mixer = "ok".to_string();
                }
            }
            Err(err) => {
                eprintln!("radiod: mixer: {err}");
                status.lock().expect("status lock poisoned").mixer = format!("error: {err}");
                return AirplayEnd::Fatal;
            }
        }
    }

    let spec = source.spec();
    // One stage for the whole session: the AirPlay source reads with a
    // timeout and substitutes silence, so it cannot silently block the
    // loop — the counters and ages below still flow per chunk.
    shared
        .debug
        .airplay_session_started(spec.rate, spec.channels);
    {
        let mut status = status.lock().expect("status lock poisoned");
        status.state = State::Playing;
        status.source = AudioSource::Airplay;
        status.airplay = Some(AirplayInfo {
            rate: spec.rate,
            channels: spec.channels,
        });
        status.playlist_url = None;
        status.stream_url = None;
        status.icy_title = None;
        status.icy_name = None;
    }

    if let Err(err) = sink.open(spec) {
        eprintln!("radiod: cannot open audio sink: {err}");
        shared.debug.sink_error(&err.to_string());
        return AirplayEnd::Fatal;
    }

    let mut buf = vec![0i16; CHUNK_SAMPLES];
    let outcome = loop {
        match rx.try_recv() {
            Ok(command) => break AirplayEnd::Command(command),
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => break AirplayEnd::Disconnected,
        }

        let n = match source.read(&mut buf) {
            Ok(0) => {
                shared.debug.event("airplay", "session ended".to_string());
                break AirplayEnd::Ended;
            }
            Ok(n) => n,
            Err(err) => {
                eprintln!("radiod: airplay source error: {err}");
                shared
                    .debug
                    .event("airplay", format!("source error: {err}"));
                break AirplayEnd::Ended;
            }
        };
        shared.debug.note_read(n);

        // Master volume × the sender's slider; apply_gain clamps, so the
        // combination can attenuate but never amplify.
        let gain = {
            let status = status.lock().expect("status lock poisoned");
            volume::gain(status.volume, status.muted) * status.airplay_gain
        };
        volume::apply_gain(&mut buf[..n], gain);

        if let Err(err) = sink.write(&buf[..n]) {
            eprintln!("radiod: error writing to audio sink: {err}");
            shared.debug.sink_error(&err.to_string());
            break AirplayEnd::Fatal;
        }
        shared.debug.note_write(n);
    };

    sink.close();
    outcome
}

fn set_stopped(status: &Mutex<Status>) {
    let mut status = status.lock().expect("status lock poisoned");
    status.state = State::Stopped;
    status.playlist_url = None;
    status.stream_url = None;
    status.icy_title = None;
    status.icy_name = None;
    status.source = AudioSource::Radio;
    status.airplay = None;
    status.airplay_gain = 1.0;
    status.airplay_track = None;
    status.airplay_artwork = None;
}

/// Paused keeps the station URLs visible — that is the difference from
/// stopped.
fn set_paused(status: &Mutex<Status>) {
    let mut status = status.lock().expect("status lock poisoned");
    status.state = State::Paused;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::sink::testing::TestSink;
    use crate::source::{SineSource, SourceError};
    use std::time::Duration;

    fn wait_for<F: Fn() -> bool>(predicate: F) {
        for _ in 0..500 {
            if predicate() {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("condition not reached within 1s");
    }

    fn sine_factory() -> SourceFactory {
        Box::new(|_| Ok(Box::new(SineSource::new())))
    }

    fn playing_status(config_toml: &str) -> Arc<Mutex<Status>> {
        let config = Config::from_toml(config_toml).unwrap();
        Arc::new(Mutex::new(Status::initial(&config)))
    }

    fn test_debug() -> Arc<DebugState> {
        Arc::new(DebugState::new())
    }

    #[test]
    fn play_and_stop_transition_status() {
        let status = playing_status("");
        let sink = TestSink::default();
        let player = spawn(status.clone(), Box::new(sink.clone()), sine_factory());

        player.send(Command::Play {
            playlist_url: "https://example.com/test.pls".to_string(),
            stream_url: "https://example.com/stream".to_string(),
        });
        wait_for(|| status.lock().unwrap().state == State::Playing);
        {
            let status = status.lock().unwrap();
            assert_eq!(
                status.playlist_url.as_deref(),
                Some("https://example.com/test.pls")
            );
            assert_eq!(
                status.stream_url.as_deref(),
                Some("https://example.com/stream")
            );
        }

        player.send(Command::Stop);
        wait_for(|| status.lock().unwrap().state == State::Stopped);
        {
            let status = status.lock().unwrap();
            assert_eq!(status.playlist_url, None);
            assert_eq!(status.stream_url, None);
        }
        wait_for(|| *sink.closed.lock().unwrap());
        assert!(!sink.samples.lock().unwrap().is_empty());
    }

    #[test]
    fn play_while_playing_switches_station() {
        let status = playing_status("");
        let sink = TestSink::default();
        let player = spawn(status.clone(), Box::new(sink.clone()), sine_factory());

        player.send(Command::Play {
            playlist_url: "https://example.com/one.pls".to_string(),
            stream_url: "https://example.com/one".to_string(),
        });
        wait_for(|| {
            status.lock().unwrap().stream_url.as_deref() == Some("https://example.com/one")
        });

        player.send(Command::Play {
            playlist_url: "https://example.com/two.pls".to_string(),
            stream_url: "https://example.com/two".to_string(),
        });
        wait_for(|| {
            status.lock().unwrap().stream_url.as_deref() == Some("https://example.com/two")
        });
        assert_eq!(status.lock().unwrap().state, State::Playing);

        player.send(Command::Stop);
        wait_for(|| status.lock().unwrap().state == State::Stopped);
    }

    fn test_tuning() -> Tuning {
        Tuning {
            initial_backoff: Duration::from_millis(5),
            max_backoff: Duration::from_millis(40),
            stable_after: Duration::from_millis(30),
        }
    }

    /// A source whose read always reports the rw_timeout stall — the
    /// field failure the fix turns into a reconnect.
    struct TimingOutSource;

    impl crate::source::Source for TimingOutSource {
        fn spec(&self) -> crate::sink::AudioSpec {
            crate::sink::AudioSpec {
                rate: 44100,
                channels: 2,
            }
        }

        fn read(&mut self, _buf: &mut [i16]) -> Result<usize, SourceError> {
            std::thread::sleep(Duration::from_millis(2));
            Err(SourceError::timeout("Operation timed out"))
        }
    }

    #[test]
    fn a_read_timeout_is_recorded_distinctly_and_reconnects() {
        let status = playing_status("");
        let sink = TestSink::default();
        let debug = test_debug();
        let opens = Arc::new(Mutex::new(0u32));
        let opens_in_factory = opens.clone();
        // First session stalls (times out); the reconnect brings up audio.
        let factory: SourceFactory = Box::new(move |_| {
            let mut opens = opens_in_factory.lock().unwrap();
            *opens += 1;
            if *opens == 1 {
                Ok(Box::new(TimingOutSource))
            } else {
                Ok(Box::new(SineSource::new()))
            }
        });
        let (player, _) = spawn_with_tuning(
            status.clone(),
            Box::new(sink.clone()),
            None,
            factory,
            test_tuning(),
            true,
            debug.clone(),
        );
        start_playing(&player, &status);

        // The stall is not fatal: the player reconnects and audio flows.
        wait_for(|| !sink.samples.lock().unwrap().is_empty());
        assert!(*opens.lock().unwrap() >= 2);

        // And /debug names it a timeout, not a plain read error or an EOF.
        let kinds: Vec<&str> = debug.snapshot().events.iter().map(|e| e.kind).collect();
        assert!(
            kinds.contains(&"read_timeout"),
            "expected a read_timeout event, got {kinds:?}"
        );
        assert!(
            !kinds.contains(&"read_error"),
            "a timeout must not be logged as a plain read_error: {kinds:?}"
        );

        player.send(Command::Stop);
        wait_for(|| status.lock().unwrap().state == State::Stopped);
    }

    /// A source that produces audio (with real-time pacing via a short
    /// sleep per read) and ends after `duration`.
    struct TimedSource {
        started: Option<Instant>,
        duration: Duration,
    }

    impl TimedSource {
        fn new(duration: Duration) -> TimedSource {
            TimedSource {
                started: None,
                duration,
            }
        }
    }

    impl crate::source::Source for TimedSource {
        fn spec(&self) -> crate::sink::AudioSpec {
            crate::sink::AudioSpec {
                rate: 44100,
                channels: 2,
            }
        }

        fn read(&mut self, buf: &mut [i16]) -> Result<usize, SourceError> {
            let started = *self.started.get_or_insert_with(Instant::now);
            if started.elapsed() >= self.duration {
                return Ok(0); // EOF
            }
            std::thread::sleep(Duration::from_millis(2));
            buf.fill(1000);
            Ok(buf.len())
        }
    }

    /// A sine source that emits ICY metadata changes on a schedule of
    /// (after_n_reads, title) entries.
    struct MetadataSource {
        inner: SineSource,
        reads: usize,
        schedule: Vec<(usize, &'static str)>,
        emitted: usize,
    }

    impl crate::source::Source for MetadataSource {
        fn spec(&self) -> crate::sink::AudioSpec {
            self.inner.spec()
        }

        fn read(&mut self, buf: &mut [i16]) -> Result<usize, SourceError> {
            self.reads += 1;
            // Pace the reads so a scheduled title stays observable for a
            // while before the next one replaces it.
            std::thread::sleep(Duration::from_millis(1));
            self.inner.read(buf)
        }

        fn icy(&mut self) -> Option<crate::icy::IcyMetadata> {
            let (after, title) = *self.schedule.get(self.emitted)?;
            if self.reads < after {
                return None;
            }
            self.emitted += 1;
            Some(crate::icy::IcyMetadata {
                name: Some("Test FM".to_string()),
                title: Some(title.to_string()),
            })
        }
    }

    #[test]
    fn metadata_changes_reach_the_status() {
        let status = playing_status("");
        let sink = TestSink::default();
        let factory: SourceFactory = Box::new(|_| {
            Ok(Box::new(MetadataSource {
                inner: SineSource::new(),
                reads: 0,
                schedule: vec![(1, "First Song"), (150, "Second Song")],
                emitted: 0,
            }))
        });
        let player = spawn(status.clone(), Box::new(sink.clone()), factory);
        start_playing(&player, &status);

        wait_for(|| status.lock().unwrap().icy_title.as_deref() == Some("First Song"));
        assert_eq!(status.lock().unwrap().icy_name.as_deref(), Some("Test FM"));
        wait_for(|| status.lock().unwrap().icy_title.as_deref() == Some("Second Song"));

        // Pause keeps the metadata — the station has not changed.
        player.send(Command::Pause);
        wait_for(|| status.lock().unwrap().state == State::Paused);
        {
            let status = status.lock().unwrap();
            assert_eq!(status.icy_title.as_deref(), Some("Second Song"));
            assert_eq!(status.icy_name.as_deref(), Some("Test FM"));
        }

        // Stop clears it.
        player.send(Command::Stop);
        wait_for(|| status.lock().unwrap().state == State::Stopped);
        {
            let status = status.lock().unwrap();
            assert_eq!(status.icy_title, None);
            assert_eq!(status.icy_name, None);
        }
    }

    #[test]
    fn station_switch_clears_stale_metadata() {
        let status = playing_status("");
        let sink = TestSink::default();
        let opens = Arc::new(Mutex::new(0u32));
        let opens_in_factory = opens.clone();
        let factory: SourceFactory = Box::new(move |_| {
            let mut opens = opens_in_factory.lock().unwrap();
            *opens += 1;
            if *opens == 1 {
                // First station reports metadata immediately.
                Ok(Box::new(MetadataSource {
                    inner: SineSource::new(),
                    reads: 0,
                    schedule: vec![(1, "Old Song")],
                    emitted: 0,
                }))
            } else {
                // Second station never reports any.
                Ok(Box::new(SineSource::new()))
            }
        });
        let player = spawn(status.clone(), Box::new(sink.clone()), factory);
        start_playing(&player, &status);
        wait_for(|| status.lock().unwrap().icy_title.as_deref() == Some("Old Song"));

        player.send(Command::Play {
            playlist_url: "https://example.com/other.pls".to_string(),
            stream_url: "https://example.com/other".to_string(),
        });
        wait_for(|| {
            status.lock().unwrap().playlist_url.as_deref() == Some("https://example.com/other.pls")
        });
        wait_for(|| status.lock().unwrap().icy_title.is_none());
        assert_eq!(status.lock().unwrap().icy_name, None);

        player.send(Command::Stop);
        wait_for(|| status.lock().unwrap().state == State::Stopped);
    }

    #[test]
    fn failing_source_keeps_retrying_until_stopped() {
        let status = playing_status("");
        let sink = TestSink::default();
        let opens = Arc::new(Mutex::new(0u32));
        let opens_in_factory = opens.clone();
        let factory: SourceFactory = Box::new(move |_| {
            *opens_in_factory.lock().unwrap() += 1;
            Err(SourceError::new("cannot connect".to_string()))
        });
        let (player, _) = spawn_with_tuning(
            status.clone(),
            Box::new(sink.clone()),
            None,
            factory,
            test_tuning(),
            true,
            test_debug(),
        );

        player.send(Command::Play {
            playlist_url: "https://example.com/bad.pls".to_string(),
            stream_url: "https://example.com/bad".to_string(),
        });
        // State is `playing` while retrying — a radio rides out failures.
        wait_for(|| *opens.lock().unwrap() >= 3);
        assert_eq!(status.lock().unwrap().state, State::Playing);

        player.send(Command::Stop);
        wait_for(|| status.lock().unwrap().state == State::Stopped);
        assert!(sink.samples.lock().unwrap().is_empty());
    }

    #[test]
    fn transient_source_failure_reconnects() {
        let status = playing_status("");
        let sink = TestSink::default();
        let opens = Arc::new(Mutex::new(0u32));
        let opens_in_factory = opens.clone();
        let factory: SourceFactory = Box::new(move |_| {
            let mut opens = opens_in_factory.lock().unwrap();
            *opens += 1;
            if *opens <= 2 {
                Err(SourceError::new("connection refused".to_string()))
            } else {
                Ok(Box::new(SineSource::new()))
            }
        });
        let (player, _) = spawn_with_tuning(
            status.clone(),
            Box::new(sink.clone()),
            None,
            factory,
            test_tuning(),
            true,
            test_debug(),
        );

        player.send(Command::Play {
            playlist_url: "https://example.com/test.pls".to_string(),
            stream_url: "https://example.com/stream".to_string(),
        });
        // Two failures, then audio flows on the third attempt.
        wait_for(|| !sink.samples.lock().unwrap().is_empty());
        assert_eq!(*opens.lock().unwrap(), 3);
        assert_eq!(status.lock().unwrap().state, State::Playing);

        player.send(Command::Stop);
        wait_for(|| status.lock().unwrap().state == State::Stopped);
    }

    #[test]
    fn source_eof_triggers_reconnect() {
        let status = playing_status("");
        let sink = TestSink::default();
        let opens = Arc::new(Mutex::new(0u32));
        let opens_in_factory = opens.clone();
        let factory: SourceFactory = Box::new(move |_| {
            *opens_in_factory.lock().unwrap() += 1;
            Ok(Box::new(TimedSource::new(Duration::from_millis(10))))
        });
        let (player, _) = spawn_with_tuning(
            status.clone(),
            Box::new(sink.clone()),
            None,
            factory,
            test_tuning(),
            true,
            test_debug(),
        );

        player.send(Command::Play {
            playlist_url: "https://example.com/test.pls".to_string(),
            stream_url: "https://example.com/stream".to_string(),
        });
        // Each source ends after 10 ms; the player must keep reconnecting.
        wait_for(|| *opens.lock().unwrap() >= 3);
        assert_eq!(status.lock().unwrap().state, State::Playing);
        assert!(!sink.samples.lock().unwrap().is_empty());

        player.send(Command::Stop);
        wait_for(|| status.lock().unwrap().state == State::Stopped);
    }

    #[test]
    fn stop_interrupts_a_long_backoff_immediately() {
        let status = playing_status("");
        let sink = TestSink::default();
        let opens = Arc::new(Mutex::new(0u32));
        let opens_in_factory = opens.clone();
        let factory: SourceFactory = Box::new(move |_| {
            *opens_in_factory.lock().unwrap() += 1;
            Err(SourceError::new("down".to_string()))
        });
        let tuning = Tuning {
            initial_backoff: Duration::from_secs(30),
            ..test_tuning()
        };
        let (player, _) = spawn_with_tuning(
            status.clone(),
            Box::new(sink.clone()),
            None,
            factory,
            tuning,
            true,
            test_debug(),
        );

        player.send(Command::Play {
            playlist_url: "https://example.com/bad.pls".to_string(),
            stream_url: "https://example.com/bad".to_string(),
        });
        wait_for(|| *opens.lock().unwrap() == 1);

        // The player is now in a 30 s backoff sleep; /stop must not wait it out.
        let before = Instant::now();
        player.send(Command::Stop);
        wait_for(|| status.lock().unwrap().state == State::Stopped);
        assert!(before.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn backoff_escalates_between_attempts() {
        let status = playing_status("");
        let sink = TestSink::default();
        let times = Arc::new(Mutex::new(Vec::<Instant>::new()));
        let times_in_factory = times.clone();
        let factory: SourceFactory = Box::new(move |_| {
            times_in_factory.lock().unwrap().push(Instant::now());
            Err(SourceError::new("down".to_string()))
        });
        let tuning = Tuning {
            initial_backoff: Duration::from_millis(20),
            max_backoff: Duration::from_millis(80),
            stable_after: Duration::from_secs(60),
        };
        let (player, _) = spawn_with_tuning(
            status.clone(),
            Box::new(sink.clone()),
            None,
            factory,
            tuning,
            true,
            test_debug(),
        );

        player.send(Command::Play {
            playlist_url: "https://example.com/bad.pls".to_string(),
            stream_url: "https://example.com/bad".to_string(),
        });
        wait_for(|| times.lock().unwrap().len() >= 4);
        player.send(Command::Stop);

        // recv_timeout guarantees at least the backoff elapses, so lower
        // bounds are deterministic: 20, 40, 80 ms between attempts.
        let times = times.lock().unwrap();
        let gap = |i: usize| times[i + 1].duration_since(times[i]);
        assert!(gap(0) >= Duration::from_millis(20), "gap 0: {:?}", gap(0));
        assert!(gap(1) >= Duration::from_millis(40), "gap 1: {:?}", gap(1));
        assert!(gap(2) >= Duration::from_millis(80), "gap 2: {:?}", gap(2));
    }

    #[test]
    fn stable_playback_resets_the_backoff() {
        let status = playing_status("");
        let sink = TestSink::default();
        let times = Arc::new(Mutex::new(Vec::<Instant>::new()));
        let times_in_factory = times.clone();
        let factory: SourceFactory = Box::new(move |_| {
            let mut times = times_in_factory.lock().unwrap();
            times.push(Instant::now());
            let attempt = times.len();
            drop(times);
            if attempt == 5 {
                // A long, stable session (longer than stable_after).
                Ok(Box::new(TimedSource::new(Duration::from_millis(60))))
            } else {
                Err(SourceError::new("down".to_string()))
            }
        });
        let tuning = Tuning {
            initial_backoff: Duration::from_millis(10),
            max_backoff: Duration::from_millis(400),
            stable_after: Duration::from_millis(30),
        };
        let (player, _) = spawn_with_tuning(
            status.clone(),
            Box::new(sink.clone()),
            None,
            factory,
            tuning,
            true,
            test_debug(),
        );

        player.send(Command::Play {
            playlist_url: "https://example.com/test.pls".to_string(),
            stream_url: "https://example.com/stream".to_string(),
        });
        wait_for(|| times.lock().unwrap().len() >= 6);
        player.send(Command::Stop);

        // Four failures escalate the backoff to 160 ms. Attempt 5 plays
        // stably for 60 ms, which must reset the backoff: the gap from
        // attempt 5 to 6 is ~60 ms (session) + ~10 ms (initial backoff),
        // far below the ~60 + 320 ms an unreset backoff would take.
        let times = times.lock().unwrap();
        let gap = times[5].duration_since(times[4]);
        assert!(gap < Duration::from_millis(200), "gap was {gap:?}");
    }

    fn counting_sine_factory() -> (SourceFactory, Arc<Mutex<u32>>) {
        let opens = Arc::new(Mutex::new(0u32));
        let counter = opens.clone();
        let factory: SourceFactory = Box::new(move |_| {
            *counter.lock().unwrap() += 1;
            Ok(Box::new(SineSource::new()))
        });
        (factory, opens)
    }

    fn start_playing(player: &Player, status: &Arc<Mutex<Status>>) {
        player.send(Command::Play {
            playlist_url: "https://example.com/test.pls".to_string(),
            stream_url: "https://example.com/stream".to_string(),
        });
        wait_for(|| status.lock().unwrap().state == State::Playing);
    }

    #[test]
    fn pause_keeps_station_and_closes_sink() {
        let status = playing_status("");
        let sink = TestSink::default();
        let player = spawn(status.clone(), Box::new(sink.clone()), sine_factory());
        start_playing(&player, &status);

        player.send(Command::Pause);
        wait_for(|| status.lock().unwrap().state == State::Paused);
        {
            let status = status.lock().unwrap();
            assert_eq!(
                status.playlist_url.as_deref(),
                Some("https://example.com/test.pls")
            );
            assert_eq!(
                status.stream_url.as_deref(),
                Some("https://example.com/stream")
            );
        }
        wait_for(|| *sink.closed.lock().unwrap());
    }

    #[test]
    fn resume_reconnects_to_the_remembered_station() {
        let status = playing_status("");
        let sink = TestSink::default();
        let (factory, opens) = counting_sine_factory();
        let player = spawn(status.clone(), Box::new(sink.clone()), factory);
        start_playing(&player, &status);
        assert_eq!(*opens.lock().unwrap(), 1);

        player.send(Command::Pause);
        wait_for(|| status.lock().unwrap().state == State::Paused);

        player.send(Command::Resume);
        wait_for(|| status.lock().unwrap().state == State::Playing);
        assert_eq!(*opens.lock().unwrap(), 2);
        assert_eq!(
            status.lock().unwrap().stream_url.as_deref(),
            Some("https://example.com/stream")
        );

        player.send(Command::Stop);
        wait_for(|| status.lock().unwrap().state == State::Stopped);
    }

    #[test]
    fn play_while_paused_switches_station() {
        let status = playing_status("");
        let sink = TestSink::default();
        let player = spawn(status.clone(), Box::new(sink.clone()), sine_factory());
        start_playing(&player, &status);

        player.send(Command::Pause);
        wait_for(|| status.lock().unwrap().state == State::Paused);

        player.send(Command::Play {
            playlist_url: "https://example.com/other.pls".to_string(),
            stream_url: "https://example.com/other".to_string(),
        });
        wait_for(|| status.lock().unwrap().state == State::Playing);
        assert_eq!(
            status.lock().unwrap().playlist_url.as_deref(),
            Some("https://example.com/other.pls")
        );
    }

    #[test]
    fn stop_while_paused_clears_station() {
        let status = playing_status("");
        let sink = TestSink::default();
        let player = spawn(status.clone(), Box::new(sink.clone()), sine_factory());
        start_playing(&player, &status);

        player.send(Command::Pause);
        wait_for(|| status.lock().unwrap().state == State::Paused);

        player.send(Command::Stop);
        wait_for(|| status.lock().unwrap().state == State::Stopped);
        let status = status.lock().unwrap();
        assert_eq!(status.playlist_url, None);
        assert_eq!(status.stream_url, None);
    }

    #[test]
    fn mixer_is_asserted_at_every_session_start() {
        use crate::mixer::testing::TestMixer;

        let status = playing_status("");
        let sink = TestSink::default();
        let mixer = TestMixer::default();
        let (player, _) = spawn_with_tuning(
            status.clone(),
            Box::new(sink.clone()),
            Some(Box::new(mixer.clone())),
            sine_factory(),
            Tuning::default(),
            true,
            test_debug(),
        );

        start_playing(&player, &status);
        assert_eq!(*mixer.asserts.lock().unwrap(), 1);
        assert_eq!(status.lock().unwrap().mixer, "ok");

        // Pause tears the session down; resume starts a new one — the
        // ceiling must be re-asserted before audio flows again.
        player.send(Command::Pause);
        wait_for(|| status.lock().unwrap().state == State::Paused);
        player.send(Command::Resume);
        wait_for(|| status.lock().unwrap().state == State::Playing);
        assert_eq!(*mixer.asserts.lock().unwrap(), 2);

        player.send(Command::Stop);
        wait_for(|| status.lock().unwrap().state == State::Stopped);
    }

    #[test]
    fn mixer_failure_refuses_to_start_audio() {
        use crate::mixer::testing::TestMixer;

        let status = playing_status("");
        let sink = TestSink::default();
        let mixer = TestMixer::default();
        *mixer.fail_with.lock().unwrap() = Some("ceiling gone".to_string());
        let (player, _) = spawn_with_tuning(
            status.clone(),
            Box::new(sink.clone()),
            Some(Box::new(mixer.clone())),
            sine_factory(),
            Tuning::default(),
            true,
            test_debug(),
        );

        player.send(Command::Play {
            playlist_url: "https://example.com/test.pls".to_string(),
            stream_url: "https://example.com/stream".to_string(),
        });
        wait_for(|| status.lock().unwrap().mixer.starts_with("error:"));
        wait_for(|| status.lock().unwrap().state == State::Stopped);
        {
            let status = status.lock().unwrap();
            assert_eq!(status.mixer, "error: ceiling gone");
        }
        // No ceiling, no audio: the sink was never even opened.
        assert!(sink.opened_with.lock().unwrap().is_none());
        assert!(sink.samples.lock().unwrap().is_empty());

        // Clearing the failure lets the next /play recover — and flips the
        // status back to ok.
        *mixer.fail_with.lock().unwrap() = None;
        start_playing(&player, &status);
        assert_eq!(status.lock().unwrap().mixer, "ok");
        player.send(Command::Stop);
        wait_for(|| status.lock().unwrap().state == State::Stopped);
    }

    fn start_airplay(player: &Player, status: &Arc<Mutex<Status>>) -> crate::airplay::BridgeSink {
        let (bridge_sink, source) = crate::airplay::bridge(44100, 2);
        player.send(Command::AirplayStarted { source });
        wait_for(|| status.lock().unwrap().source == AudioSource::Airplay);
        bridge_sink
    }

    #[test]
    fn airplay_preempts_the_station_and_the_radio_resumes_after() {
        use openairplay2::AudioSink as _;

        let status = playing_status("");
        let sink = TestSink::default();
        let (factory, opens) = counting_sine_factory();
        let player = spawn(status.clone(), Box::new(sink.clone()), factory);
        start_playing(&player, &status);
        assert_eq!(*opens.lock().unwrap(), 1);

        let mut bridge_sink = start_airplay(&player, &status);
        {
            let status = status.lock().unwrap();
            assert_eq!(status.state, State::Playing);
            assert_eq!(status.playlist_url, None, "station hidden during airplay");
            let info = status.airplay.expect("airplay info");
            assert_eq!((info.rate, info.channels), (44100, 2));
        }

        // Bridged PCM reaches the ordinary sink.
        let mark = sink.samples.lock().unwrap().len();
        bridge_sink.write(&[1000i16; 512]);
        wait_for(|| {
            let samples = sink.samples.lock().unwrap();
            samples[mark.min(samples.len())..].iter().any(|s| *s != 0)
        });

        // Sender goes away: the library drops its sink, the bridge reports
        // EOF, and the remembered station comes back (resume_radio = true).
        drop(bridge_sink);
        wait_for(|| status.lock().unwrap().source == AudioSource::Radio);
        wait_for(|| status.lock().unwrap().state == State::Playing);
        wait_for(|| *opens.lock().unwrap() == 2);
        assert_eq!(
            status.lock().unwrap().playlist_url.as_deref(),
            Some("https://example.com/test.pls")
        );
        assert_eq!(status.lock().unwrap().airplay, None);

        player.send(Command::Stop);
        wait_for(|| status.lock().unwrap().state == State::Stopped);
    }

    #[test]
    fn airplay_end_without_resume_radio_goes_idle() {
        let status = playing_status("");
        let sink = TestSink::default();
        let (player, _) = spawn_with_tuning(
            status.clone(),
            Box::new(sink.clone()),
            None,
            sine_factory(),
            Tuning::default(),
            false,
            test_debug(),
        );
        start_playing(&player, &status);

        let bridge_sink = start_airplay(&player, &status);
        drop(bridge_sink);
        wait_for(|| status.lock().unwrap().state == State::Stopped);
        let status = status.lock().unwrap();
        assert_eq!(status.source, AudioSource::Radio);
        assert_eq!(status.playlist_url, None, "station is not resumed");
    }

    #[test]
    fn airplay_sender_volume_scales_the_bridged_samples() {
        use openairplay2::AudioSink as _;

        let status = playing_status("initial_volume = 100");
        let sink = TestSink::default();
        let player = spawn(status.clone(), Box::new(sink.clone()), sine_factory());

        let mut bridge_sink = start_airplay(&player, &status);
        // Sender volume is shared state (set by the event task in
        // production), read by the playback loop per chunk — a slider
        // drag must not interrupt the session.
        status.lock().unwrap().airplay_gain = 0.1;

        let mark = sink.samples.lock().unwrap().len();
        bridge_sink.write(&[i16::MAX; 2048]);
        wait_for(|| {
            let samples = sink.samples.lock().unwrap();
            samples[mark.min(samples.len())..].iter().any(|s| *s != 0)
        });
        let samples = sink.samples.lock().unwrap();
        let peak = samples[mark..].iter().map(|s| i32::from(*s).abs()).max();
        let bound = (0.1 * f32::from(i16::MAX)) as i32 + 1;
        let peak = peak.unwrap();
        assert!(peak > 0 && peak <= bound, "peak {peak} vs bound {bound}");
    }

    #[test]
    fn stop_during_airplay_silences_and_forgets_the_station() {
        let status = playing_status("");
        let sink = TestSink::default();
        let player = spawn(status.clone(), Box::new(sink.clone()), sine_factory());
        start_playing(&player, &status);

        let _bridge_sink = start_airplay(&player, &status);
        player.send(Command::Stop);
        wait_for(|| status.lock().unwrap().state == State::Stopped);
        let status = status.lock().unwrap();
        assert_eq!(status.source, AudioSource::Radio);
        assert_eq!(status.airplay, None);
        assert_eq!(
            status.playlist_url, None,
            "no resume after an explicit stop"
        );
    }

    #[test]
    fn resume_keeps_the_station_name_and_clears_the_song() {
        let status = playing_status("");
        let sink = TestSink::default();
        let opens = Arc::new(Mutex::new(0u32));
        let opens_in_factory = opens.clone();
        // Metadata only on the first connection: on resume, anything still
        // visible must have been *kept*, not repopulated.
        let factory: SourceFactory = Box::new(move |_| {
            let mut opens = opens_in_factory.lock().unwrap();
            *opens += 1;
            if *opens == 1 {
                Ok(Box::new(MetadataSource {
                    inner: SineSource::new(),
                    reads: 0,
                    schedule: vec![(1, "First Song")],
                    emitted: 0,
                }))
            } else {
                Ok(Box::new(SineSource::new()))
            }
        });
        let player = spawn(status.clone(), Box::new(sink.clone()), factory);
        start_playing(&player, &status);
        wait_for(|| status.lock().unwrap().icy_title.as_deref() == Some("First Song"));

        player.send(Command::Pause);
        wait_for(|| status.lock().unwrap().state == State::Paused);
        player.send(Command::Resume);
        wait_for(|| status.lock().unwrap().state == State::Playing);
        wait_for(|| *opens.lock().unwrap() == 2);
        {
            let status = status.lock().unwrap();
            assert_eq!(
                status.icy_name.as_deref(),
                Some("Test FM"),
                "same station: the name survives the resume"
            );
            assert_eq!(status.icy_title, None, "the song does not");
        }

        player.send(Command::Stop);
        wait_for(|| status.lock().unwrap().state == State::Stopped);
    }

    #[test]
    fn dropping_all_handles_stops_the_thread_and_closes_the_sink() {
        let status = playing_status("");
        let sink = TestSink::default();
        let (player, handle) = spawn_with_tuning(
            status.clone(),
            Box::new(sink.clone()),
            None,
            sine_factory(),
            Tuning::default(),
            true,
            test_debug(),
        );
        start_playing(&player, &status);

        // Closing the command channel is the shutdown path: the play loop
        // notices the disconnect, closes the sink, and the thread exits.
        drop(player);
        handle.join().expect("player thread panicked");
        assert!(*sink.closed.lock().unwrap());
    }

    #[test]
    fn samples_written_respect_the_volume() {
        // Full-scale sine through the pipeline at volume 50: no sample may
        // exceed half of full scale. (The hardware ceiling lives in the
        // mixer now; this guards the software half — gain never amplifies.)
        let status = playing_status("initial_volume = 50");
        assert_eq!(status.lock().unwrap().volume, 50);
        let sink = TestSink::default();
        let player = spawn(status.clone(), Box::new(sink.clone()), sine_factory());

        player.send(Command::Play {
            playlist_url: "https://example.com/test.pls".to_string(),
            stream_url: "https://example.com/stream".to_string(),
        });
        wait_for(|| sink.samples.lock().unwrap().len() >= CHUNK_SAMPLES);
        player.send(Command::Stop);
        wait_for(|| status.lock().unwrap().state == State::Stopped);

        let samples = sink.samples.lock().unwrap();
        let bound = (0.5 * f32::from(i16::MAX)) as i32;
        let peak = samples.iter().map(|s| i32::from(*s).abs()).max().unwrap();
        assert!(peak <= bound, "peak {peak} exceeds bound {bound}");
        // And the audio is not silence — the gain scaled it, not zeroed it.
        assert!(peak > bound / 2, "peak {peak} suspiciously low");
    }

    #[test]
    fn muted_status_produces_silence() {
        let status = playing_status("");
        status.lock().unwrap().muted = true;
        let sink = TestSink::default();
        let player = spawn(status.clone(), Box::new(sink.clone()), sine_factory());

        player.send(Command::Play {
            playlist_url: "https://example.com/test.pls".to_string(),
            stream_url: "https://example.com/stream".to_string(),
        });
        wait_for(|| sink.samples.lock().unwrap().len() >= CHUNK_SAMPLES);
        player.send(Command::Stop);

        let samples = sink.samples.lock().unwrap();
        assert!(samples.iter().all(|s| *s == 0));
    }

    /// A source that plays one chunk, then blocks inside read() until the
    /// test releases it (a half-open TCP connection in miniature), then
    /// reports EOF.
    struct BlockingSource {
        played_first: bool,
        release: Receiver<()>,
    }

    impl crate::source::Source for BlockingSource {
        fn spec(&self) -> crate::sink::AudioSpec {
            crate::sink::AudioSpec {
                rate: 44100,
                channels: 2,
            }
        }

        fn read(&mut self, buf: &mut [i16]) -> Result<usize, SourceError> {
            if !self.played_first {
                self.played_first = true;
                buf.fill(1000);
                return Ok(buf.len());
            }
            let _ = self.release.recv();
            Ok(0)
        }
    }

    #[test]
    fn blocked_read_is_visible_and_stalls_once() {
        use crate::debug::{StallTransition, check_stall};

        let status = playing_status("");
        let sink = TestSink::default();
        let debug = test_debug();
        let (release_tx, release_rx) = channel();
        let release_rx = Arc::new(Mutex::new(Some(release_rx)));
        let opens = Arc::new(Mutex::new(0u32));
        let opens_in_factory = opens.clone();
        let factory: SourceFactory = Box::new(move |_| {
            let mut opens = opens_in_factory.lock().unwrap();
            *opens += 1;
            if *opens == 1 {
                Ok(Box::new(BlockingSource {
                    played_first: false,
                    release: release_rx.lock().unwrap().take().unwrap(),
                }))
            } else {
                Ok(Box::new(SineSource::new()))
            }
        });
        let (player, _) = spawn_with_tuning(
            status.clone(),
            Box::new(sink.clone()),
            None,
            factory,
            test_tuning(),
            true,
            debug.clone(),
        );
        start_playing(&player, &status);

        // The first chunk flows, then the read blocks forever: the stage
        // was written before the call, so the wedge is observable.
        wait_for(|| debug.snapshot().samples_written > 0);
        wait_for(|| debug.snapshot().stage == Stage::Reading);

        // The monitor flags the stall exactly once, with the stuck stage.
        wait_for(|| {
            matches!(
                check_stall(&status, &debug, Duration::from_millis(20)),
                Some(StallTransition::Entered {
                    stage: Stage::Reading,
                    ..
                })
            )
        });
        assert!(
            check_stall(&status, &debug, Duration::from_millis(20)).is_none(),
            "no second transition while still stalled"
        );
        let snapshot = debug.snapshot();
        assert!(snapshot.stalled);
        assert_eq!(snapshot.stage, Stage::Reading);
        assert!(snapshot.last_write_ms_ago.unwrap() >= 20);

        // Unblocking the read ends the source (EOF), the reconnect loop
        // brings up the sine source, audio flows: the stall ends.
        release_tx.send(()).unwrap();
        wait_for(|| {
            matches!(
                check_stall(&status, &debug, Duration::from_millis(20)),
                Some(StallTransition::Ended { .. })
            )
        });
        assert!(!debug.snapshot().stalled);
        let kinds: Vec<&str> = debug.snapshot().events.iter().map(|e| e.kind).collect();
        for kind in ["stall", "recovered", "eof", "connect", "connected"] {
            assert!(kinds.contains(&kind), "missing {kind} in {kinds:?}");
        }

        player.send(Command::Stop);
        wait_for(|| status.lock().unwrap().state == State::Stopped);
    }

    /// A sink whose first write blocks until released — a wedged ALSA
    /// writei after a DAC hiccup.
    struct BlockingSink {
        blocked_once: bool,
        release: Receiver<()>,
    }

    impl AudioSink for BlockingSink {
        fn open(&mut self, _spec: crate::sink::AudioSpec) -> Result<(), crate::sink::SinkError> {
            Ok(())
        }

        fn write(&mut self, _frames: &[i16]) -> Result<(), crate::sink::SinkError> {
            if !self.blocked_once {
                self.blocked_once = true;
                let _ = self.release.recv();
            }
            Ok(())
        }

        fn close(&mut self) {}
    }

    #[test]
    fn blocked_write_is_visible_in_the_heartbeat() {
        use crate::debug::{StallTransition, check_stall};

        let status = playing_status("");
        let debug = test_debug();
        let (release_tx, release_rx) = channel();
        let (player, _) = spawn_with_tuning(
            status.clone(),
            Box::new(BlockingSink {
                blocked_once: false,
                release: release_rx,
            }),
            None,
            sine_factory(),
            test_tuning(),
            true,
            debug.clone(),
        );
        start_playing(&player, &status);

        wait_for(|| debug.snapshot().stage == Stage::Writing);
        std::thread::sleep(Duration::from_millis(30));
        let snapshot = debug.snapshot();
        assert_eq!(snapshot.stage, Stage::Writing, "still stuck in the write");
        assert!(snapshot.stage_ms_ago >= 30);
        assert!(snapshot.samples_read > 0, "the read side did run");
        assert_eq!(snapshot.samples_written, 0, "nothing reached the sink");
        assert!(matches!(
            check_stall(&status, &debug, Duration::from_millis(20)),
            Some(StallTransition::Entered {
                stage: Stage::Writing,
                ..
            })
        ));

        release_tx.send(()).unwrap();
        player.send(Command::Stop);
        wait_for(|| status.lock().unwrap().state == State::Stopped);
    }

    #[test]
    fn failing_reconnects_are_recorded_in_the_heartbeat() {
        let status = playing_status("");
        let debug = test_debug();
        let factory: SourceFactory =
            Box::new(|_| Err(SourceError::new("cannot connect".to_string())));
        let (player, _) = spawn_with_tuning(
            status.clone(),
            Box::new(TestSink::default()),
            None,
            factory,
            test_tuning(),
            true,
            debug.clone(),
        );
        player.send(Command::Play {
            playlist_url: "https://example.com/bad.pls".to_string(),
            stream_url: "https://example.com/bad".to_string(),
        });

        // The hypothesis-2 signature: attempts climbing, a fresh error.
        wait_for(|| debug.snapshot().connect_attempts >= 3);
        let snapshot = debug.snapshot();
        assert_eq!(
            snapshot.stream_url.as_deref(),
            Some("https://example.com/bad")
        );
        assert_eq!(
            snapshot.last_error.as_ref().unwrap().message,
            "cannot connect"
        );
        let kinds: Vec<&str> = snapshot.events.iter().map(|e| e.kind).collect();
        for kind in ["session", "connect", "connect_failed", "backoff"] {
            assert!(kinds.contains(&kind), "missing {kind} in {kinds:?}");
        }

        player.send(Command::Stop);
        wait_for(|| status.lock().unwrap().state == State::Stopped);
        wait_for(|| debug.snapshot().stage == Stage::Idle);
        assert!(
            debug
                .snapshot()
                .events
                .iter()
                .any(|e| e.kind == "command" && e.detail == "stop"),
            "processed commands land in the ring"
        );
    }

    /// A source that trickles audio far below real time (an underrunning
    /// icecast server): reads succeed, so only the rate betrays it.
    struct DribbleSource;

    impl crate::source::Source for DribbleSource {
        fn spec(&self) -> crate::sink::AudioSpec {
            crate::sink::AudioSpec {
                rate: 44100,
                channels: 2,
            }
        }

        fn read(&mut self, buf: &mut [i16]) -> Result<usize, SourceError> {
            std::thread::sleep(Duration::from_millis(5));
            let n = buf.len().min(32);
            buf[..n].fill(500);
            Ok(n)
        }
    }

    #[test]
    fn a_starving_stream_shows_in_the_sample_rate() {
        use crate::debug::{STALL_THRESHOLD, check_stall};

        let status = playing_status("");
        let sink = TestSink::default();
        let debug = test_debug();
        let factory: SourceFactory = Box::new(|_| Ok(Box::new(DribbleSource)));
        let (player, _) = spawn_with_tuning(
            status.clone(),
            Box::new(sink.clone()),
            None,
            factory,
            test_tuning(),
            true,
            debug.clone(),
        );
        start_playing(&player, &status);

        std::thread::sleep(Duration::from_millis(100));
        let snapshot = debug.snapshot();
        // The loop is alive (recent ages, no stall) but the counters run
        // far below the 88200 samples/s the spec calls for.
        assert!(snapshot.samples_written > 0);
        assert!(
            snapshot.samples_written < 4410,
            "wrote {} samples in ~100ms",
            snapshot.samples_written
        );
        assert!(snapshot.last_write_ms_ago.unwrap() < 1000);
        assert!(check_stall(&status, &debug, STALL_THRESHOLD).is_none());
        assert!(!debug.snapshot().stalled);

        player.send(Command::Stop);
        wait_for(|| status.lock().unwrap().state == State::Stopped);
    }
}
