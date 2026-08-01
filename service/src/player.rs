//! The player thread: owns the audio pipeline and is the only writer of
//! playback state transitions in the shared status.

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::sink::AudioSink;
use crate::source::SourceFactory;
use crate::status::{State, Status};
use crate::volume;

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

#[derive(Debug, Clone)]
pub enum Command {
    Play {
        playlist_url: String,
        stream_url: String,
    },
    Stop,
    Pause,
    Resume,
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

pub fn spawn(
    status: Arc<Mutex<Status>>,
    sink: Box<dyn AudioSink>,
    source_factory: SourceFactory,
) -> Player {
    spawn_with_tuning(status, sink, source_factory, Tuning::default()).0
}

/// Also returns the thread's join handle: the thread exits (closing the
/// sink) when every `Player` handle is dropped.
pub fn spawn_with_tuning(
    status: Arc<Mutex<Status>>,
    sink: Box<dyn AudioSink>,
    source_factory: SourceFactory,
    tuning: Tuning,
) -> (Player, std::thread::JoinHandle<()>) {
    let (tx, rx) = channel();
    let handle = std::thread::spawn(move || run(&rx, &status, sink, &source_factory, tuning));
    (Player { tx }, handle)
}

fn run(
    rx: &Receiver<Command>,
    status: &Mutex<Status>,
    mut sink: Box<dyn AudioSink>,
    source_factory: &SourceFactory,
    tuning: Tuning,
) {
    let mut session = Session::Idle;
    loop {
        session = match session {
            Session::Playing(station) => {
                match play_with_retries(rx, status, sink.as_mut(), source_factory, &station, tuning)
                {
                    RetryEnd::Command(command) => {
                        transition(command, Session::Playing(station), status)
                    }
                    RetryEnd::Fatal => {
                        set_stopped(status);
                        Session::Idle
                    }
                    RetryEnd::Disconnected => return,
                }
            }
            idle_or_paused => {
                let Ok(command) = rx.recv() else { return };
                transition(command, idle_or_paused, status)
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
    status: &Mutex<Status>,
    sink: &mut dyn AudioSink,
    source_factory: &SourceFactory,
    station: &Station,
    tuning: Tuning,
) -> RetryEnd {
    let mut backoff = tuning.initial_backoff;
    loop {
        let started = Instant::now();
        match play(rx, status, sink, source_factory, station) {
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
fn transition(command: Command, session: Session, status: &Mutex<Status>) -> Session {
    match command {
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
            Session::Idle => Session::Idle,
        },
        Command::Resume => match session {
            // From Playing this only happens when a Resume interrupted the
            // play loop; the pipeline was torn down, so just restart it.
            Session::Playing(station) | Session::Paused(station) => Session::Playing(station),
            Session::Idle => Session::Idle,
        },
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
    status: &Mutex<Status>,
    sink: &mut dyn AudioSink,
    source_factory: &SourceFactory,
    station: &Station,
) -> Outcome {
    // Optimistic: `playing` means "trying to play". Set before opening so
    // the status holds steady through reconnect attempts. Metadata resets
    // per session — a new station (or a reconnect) repopulates it.
    {
        let mut status = status.lock().expect("status lock poisoned");
        status.state = State::Playing;
        status.playlist_url = Some(station.playlist_url.clone());
        status.stream_url = Some(station.stream_url.clone());
        status.icy_title = None;
        status.icy_name = None;
    }

    let stream_url = station.stream_url.as_str();
    let mut source = match source_factory(stream_url) {
        Ok(source) => source,
        Err(err) => {
            eprintln!("radiod: cannot open {stream_url}: {err}");
            return Outcome::SourceEnded;
        }
    };
    if let Err(err) = sink.open(source.spec()) {
        eprintln!("radiod: cannot open audio sink: {err}");
        return Outcome::SinkFailed;
    }

    let mut buf = vec![0i16; CHUNK_SAMPLES];
    let outcome = loop {
        match rx.try_recv() {
            Ok(command) => break Outcome::Interrupted(command),
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => break Outcome::Disconnected,
        }

        let n = match source.read(&mut buf) {
            Ok(0) => {
                eprintln!("radiod: stream ended: {stream_url}");
                break Outcome::SourceEnded;
            }
            Ok(n) => n,
            Err(err) => {
                eprintln!("radiod: error reading {stream_url}: {err}");
                break Outcome::SourceEnded;
            }
        };

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

        if let Err(err) = sink.write(&buf[..n]) {
            eprintln!("radiod: error writing to audio sink: {err}");
            break Outcome::SinkFailed;
        }
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
            Err(SourceError("cannot connect".to_string()))
        });
        let (player, _) = spawn_with_tuning(
            status.clone(),
            Box::new(sink.clone()),
            factory,
            test_tuning(),
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
                Err(SourceError("connection refused".to_string()))
            } else {
                Ok(Box::new(SineSource::new()))
            }
        });
        let (player, _) = spawn_with_tuning(
            status.clone(),
            Box::new(sink.clone()),
            factory,
            test_tuning(),
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
            factory,
            test_tuning(),
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
            Err(SourceError("down".to_string()))
        });
        let tuning = Tuning {
            initial_backoff: Duration::from_secs(30),
            ..test_tuning()
        };
        let (player, _) =
            spawn_with_tuning(status.clone(), Box::new(sink.clone()), factory, tuning);

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
            Err(SourceError("down".to_string()))
        });
        let tuning = Tuning {
            initial_backoff: Duration::from_millis(20),
            max_backoff: Duration::from_millis(80),
            stable_after: Duration::from_secs(60),
        };
        let (player, _) =
            spawn_with_tuning(status.clone(), Box::new(sink.clone()), factory, tuning);

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
                Err(SourceError("down".to_string()))
            }
        });
        let tuning = Tuning {
            initial_backoff: Duration::from_millis(10),
            max_backoff: Duration::from_millis(400),
            stable_after: Duration::from_millis(30),
        };
        let (player, _) =
            spawn_with_tuning(status.clone(), Box::new(sink.clone()), factory, tuning);

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
    fn dropping_all_handles_stops_the_thread_and_closes_the_sink() {
        let status = playing_status("");
        let sink = TestSink::default();
        let (player, handle) = spawn_with_tuning(
            status.clone(),
            Box::new(sink.clone()),
            sine_factory(),
            Tuning::default(),
        );
        start_playing(&player, &status);

        // Closing the command channel is the shutdown path: the play loop
        // notices the disconnect, closes the sink, and the thread exits.
        drop(player);
        handle.join().expect("player thread panicked");
        assert!(*sink.closed.lock().unwrap());
    }

    #[test]
    fn samples_written_respect_max_volume() {
        // Full-scale sine through the pipeline with volume at the cap: no
        // sample may exceed max_volume percent of full scale.
        let status = playing_status("max_volume = 50\ninitial_volume = 100");
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
}
