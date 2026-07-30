//! The player thread: owns the audio pipeline and is the only writer of
//! playback state transitions in the shared status.

use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex};

use crate::sink::AudioSink;
use crate::source::SourceFactory;
use crate::status::{State, Status};
use crate::volume;

const CHUNK_SAMPLES: usize = 2048;

#[derive(Debug, Clone)]
pub enum Command {
    Play {
        playlist_url: String,
        stream_url: String,
    },
    Stop,
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
    let (tx, rx) = channel();
    std::thread::spawn(move || run(&rx, &status, sink, &source_factory));
    Player { tx }
}

fn run(
    rx: &Receiver<Command>,
    status: &Mutex<Status>,
    mut sink: Box<dyn AudioSink>,
    source_factory: &SourceFactory,
) {
    let mut next = rx.recv().ok();
    while let Some(command) = next.take() {
        match command {
            Command::Stop => {
                next = rx.recv().ok();
            }
            Command::Play {
                playlist_url,
                stream_url,
            } => {
                next = play(
                    rx,
                    status,
                    sink.as_mut(),
                    source_factory,
                    playlist_url,
                    stream_url,
                );
                set_stopped(status);
                if next.is_none() {
                    next = rx.recv().ok();
                }
            }
        }
    }
}

/// Plays one station until stopped, interrupted by a new command, or the
/// source ends. Returns a command that interrupted playback, if any.
fn play(
    rx: &Receiver<Command>,
    status: &Mutex<Status>,
    sink: &mut dyn AudioSink,
    source_factory: &SourceFactory,
    playlist_url: String,
    stream_url: String,
) -> Option<Command> {
    let mut source = match source_factory(&stream_url) {
        Ok(source) => source,
        Err(err) => {
            eprintln!("radiod: cannot open {stream_url}: {err}");
            return None;
        }
    };
    if let Err(err) = sink.open(source.spec()) {
        eprintln!("radiod: cannot open audio sink: {err}");
        return None;
    }

    {
        let mut status = status.lock().expect("status lock poisoned");
        status.state = State::Playing;
        status.playlist_url = Some(playlist_url);
        status.stream_url = Some(stream_url.clone());
    }

    let mut buf = vec![0i16; CHUNK_SAMPLES];
    let interrupt = loop {
        match rx.try_recv() {
            Ok(command) => break Some(command),
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => break None,
        }

        let n = match source.read(&mut buf) {
            Ok(0) => {
                eprintln!("radiod: stream ended: {stream_url}");
                break None;
            }
            Ok(n) => n,
            Err(err) => {
                eprintln!("radiod: error reading {stream_url}: {err}");
                break None;
            }
        };

        let gain = {
            let status = status.lock().expect("status lock poisoned");
            volume::gain(status.volume, status.muted)
        };
        volume::apply_gain(&mut buf[..n], gain);

        if let Err(err) = sink.write(&buf[..n]) {
            eprintln!("radiod: error writing to audio sink: {err}");
            break None;
        }
    };

    sink.close();
    interrupt
}

fn set_stopped(status: &Mutex<Status>) {
    let mut status = status.lock().expect("status lock poisoned");
    status.state = State::Stopped;
    status.playlist_url = None;
    status.stream_url = None;
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

    #[test]
    fn failing_source_returns_to_stopped() {
        let status = playing_status("");
        let sink = TestSink::default();
        let factory: SourceFactory = Box::new(|_| Err(SourceError("cannot connect".to_string())));
        let player = spawn(status.clone(), Box::new(sink.clone()), factory);

        player.send(Command::Play {
            playlist_url: "https://example.com/bad.pls".to_string(),
            stream_url: "https://example.com/bad".to_string(),
        });
        // The player must survive the failure and remain usable.
        player.send(Command::Stop);
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(status.lock().unwrap().state, State::Stopped);
        assert!(sink.samples.lock().unwrap().is_empty());
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
