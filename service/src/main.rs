mod airplay;
mod config;
mod icy;
mod mixer;
mod pipeline;
mod player;
mod pls;
mod server;
mod sink;
mod source;
mod state;
mod status;
mod volume;
mod web;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use std::time::{Duration, Instant};

use config::Config;
use pipeline::FfmpegSource;
use sink::{AudioSink, NullSink, WavSink};
use source::{Source, SourceError};
use status::{State, Status};

const USAGE: &str = "usage: radiod [--config <path>] [--sink alsa|null|wav:<path>] \
     [--web-dir <path>] [-v | --version]";

#[cfg(target_os = "linux")]
const DEFAULT_SINK: &str = "alsa";
#[cfg(not(target_os = "linux"))]
const DEFAULT_SINK: &str = "null";

struct Args {
    config_path: Option<PathBuf>,
    sink: Option<String>,
    /// Serve templates/assets from this directory instead of the embedded
    /// copies — PHP-style edit-and-reload during development.
    web_dir: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut args = std::env::args().skip(1);
    let mut config_path = None;
    let mut sink = None;
    let mut web_dir = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => {
                let path = args.next().ok_or("--config requires a path")?;
                config_path = Some(PathBuf::from(path));
            }
            "--sink" => {
                let value = args.next().ok_or("--sink requires a value")?;
                sink = Some(value);
            }
            "--web-dir" => {
                let path = args.next().ok_or("--web-dir requires a path")?;
                web_dir = Some(PathBuf::from(path));
            }
            "-v" | "--version" => {
                println!("radiod {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument {other:?}\n{USAGE}")),
        }
    }
    Ok(Args {
        config_path,
        sink,
        web_dir,
    })
}

fn load_config(args: &Args) -> Result<Config, String> {
    match &args.config_path {
        // An explicitly given config file must exist.
        Some(path) => Config::load(path).map_err(|err| format!("{}: {err}", path.display())),
        // The default path is optional so `cargo run` works out of the box.
        None => {
            let path = PathBuf::from(config::DEFAULT_CONFIG_PATH);
            if path.exists() {
                Config::load(&path).map_err(|err| format!("{}: {err}", path.display()))
            } else {
                Ok(Config::default())
            }
        }
    }
}

/// Builds the sink from --sink; defaults to alsa on Linux, null elsewhere.
fn make_sink(args: &Args, config: &Config) -> Result<Box<dyn AudioSink>, String> {
    match args.sink.as_deref().unwrap_or(DEFAULT_SINK) {
        "null" => Ok(Box::new(NullSink)),
        "alsa" => make_alsa_sink(config),
        value => match value.split_once(':') {
            Some(("wav", path)) if !path.is_empty() => {
                Ok(Box::new(WavSink::new(PathBuf::from(path))))
            }
            _ => Err(format!("unknown sink {value:?}\n{USAGE}")),
        },
    }
}

#[cfg(target_os = "linux")]
fn make_alsa_sink(config: &Config) -> Result<Box<dyn AudioSink>, String> {
    Ok(Box::new(sink::AlsaSink::new(config.audio_device.clone())))
}

#[cfg(not(target_os = "linux"))]
fn make_alsa_sink(_config: &Config) -> Result<Box<dyn AudioSink>, String> {
    Err("the alsa sink is only available on Linux".to_string())
}

fn make_source(stream_url: &str) -> Result<Box<dyn Source>, SourceError> {
    Ok(Box::new(FfmpegSource::open(stream_url)?))
}

/// The mixer that owns the hardware ceiling: required for the alsa sink
/// (playing without a ceiling is the one unacceptable outcome), absent for
/// the dev sinks.
fn make_mixer(
    args: &Args,
    config: &Config,
) -> Result<Option<Box<dyn mixer::MixerControl>>, String> {
    if args.sink.as_deref().unwrap_or(DEFAULT_SINK) != "alsa" {
        return Ok(None);
    }
    let Some(mixer_config) = config.mixer.clone() else {
        return Err(
            "the alsa sink needs a [mixer] section in the config: radiod owns \
             the hardware ceiling that protects the speakers (control = \"...\" \
             plus ceiling_db or ceiling_percent; see config.toml.example)"
                .to_string(),
        );
    };
    mixer::make_alsa_mixer(&config.audio_device, mixer_config)
        .map(Some)
        .map_err(|err| err.to_string())
}

// Current-thread flavor: the Pi Zero has one ARMv6 core, so a multi-thread
// scheduler buys nothing. Blocking work (playlist fetches) still runs on
// tokio's separate blocking pool; audio runs on its own OS thread.
#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(err) => {
            eprintln!("radiod: {err}");
            return ExitCode::from(2);
        }
    };

    let mut config = match load_config(&args) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("radiod: {err}");
            return ExitCode::FAILURE;
        }
    };

    // Saved settings win over config defaults: the radio remembers its
    // volume across restarts, reboots and reinstalls.
    let saved_state = state::load(&config.state_path);
    if let Some(volume) = saved_state.and_then(|s| s.volume) {
        println!(
            "radiod: state: restored volume {volume} from {}",
            config.state_path.display()
        );
        config.initial_volume = volume;
    }

    let sink = match make_sink(&args, &config) {
        Ok(sink) => sink,
        Err(err) => {
            eprintln!("radiod: {err}");
            return ExitCode::from(2);
        }
    };

    let mut mixer = match make_mixer(&args, &config) {
        Ok(mixer) => mixer,
        Err(err) => {
            eprintln!("radiod: {err}");
            return ExitCode::from(2);
        }
    };

    // Assert the hardware ceiling before anything can play. Refusing to
    // start beats playing at an unknown level.
    let mut initial_status = Status::initial(&config);
    if let Some(mixer) = mixer.as_mut() {
        if let Err(err) = mixer.assert_ceiling() {
            eprintln!("radiod: mixer: {err}");
            return ExitCode::FAILURE;
        }
        initial_status.mixer = "ok".to_string();
    }

    let listener = match tokio::net::TcpListener::bind(config.listen).await {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("radiod: cannot listen on {}: {err}", config.listen);
            return ExitCode::FAILURE;
        }
    };

    let status = Arc::new(Mutex::new(initial_status));
    // The player owns the mixer from here: the ceiling is re-asserted at
    // every session start, so external meddling (alsamixer, alsactl
    // restore, a re-enumerating USB DAC) is corrected before audio flows.
    let player = player::spawn_with_tuning(
        status.clone(),
        sink,
        mixer,
        Box::new(make_source),
        player::Tuning::default(),
        config.airplay.resume_radio,
    )
    .0;

    if config.airplay.enabled {
        // Identity problems are config-grade: fail fast, like any other
        // startup misconfiguration. A missing Avahi, by contrast, only
        // degrades: AirPlay stays dark and the radio still works.
        let receiver = match openairplay2::Receiver::builder()
            .name(config.airplay.name.clone())
            .port(config.airplay.port)
            .identity_path(config.airplay.identity_path.clone())
            .build()
        {
            Ok(receiver) => receiver,
            Err(err) => {
                eprintln!(
                    "radiod: airplay: cannot set up receiver identity at {}: {err}",
                    config.airplay.identity_path.display()
                );
                return ExitCode::FAILURE;
            }
        };
        let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
        let event_player = player.clone();
        let event_status = status.clone();
        tokio::spawn(async move {
            // Never reset, even by an artwork clear: an old version must
            // not come back as a different track's URL.
            let mut artwork_version: u64 = 0;
            while let Some(event) = events_rx.recv().await {
                match event {
                    // Volume is shared state, not a command: slider drags
                    // arrive as event bursts, and a command would interrupt
                    // the playback loop (tearing down and reopening ALSA)
                    // for every step. The loop reads the gain per chunk,
                    // exactly like the website's master volume.
                    openairplay2::Event::Volume { db } => {
                        event_status
                            .lock()
                            .expect("status lock poisoned")
                            .airplay_gain = airplay::db_to_gain(db);
                    }
                    // Each Metadata event is a complete statement, not a
                    // delta: replace wholesale. Senders repeat themselves
                    // (2–5× per track observed), so equality-check before
                    // touching the shared status.
                    openairplay2::Event::Metadata {
                        title,
                        artist,
                        album,
                    } => {
                        let track = (title.is_some() || artist.is_some() || album.is_some())
                            .then_some(status::AirplayTrack {
                                title,
                                artist,
                                album,
                            });
                        let mut status = event_status.lock().expect("status lock poisoned");
                        if status.airplay_track != track {
                            status.airplay_track = track;
                        }
                    }
                    // Artwork comes exactly as sent; empty data is the
                    // sender clearing it (image/none, seen mid-session at
                    // track transitions).
                    openairplay2::Event::Artwork { content_type, data } => {
                        let mut status = event_status.lock().expect("status lock poisoned");
                        status.airplay_artwork = if data.is_empty() {
                            None
                        } else {
                            artwork_version += 1;
                            Some(status::AirplayArtwork {
                                content_type,
                                data: Arc::new(data),
                                version: artwork_version,
                            })
                        };
                    }
                    openairplay2::Event::SessionEnded => {
                        event_player.send(player::Command::AirplayEnded);
                    }
                    // The playback side of SessionStarted is redundant with
                    // the sink factory, but this is the ordered place to
                    // drop the previous session's now-playing state: the
                    // event channel is FIFO, so the clear can never race
                    // past the new session's metadata replay.
                    openairplay2::Event::SessionStarted { .. } => {
                        let mut status = event_status.lock().expect("status lock poisoned");
                        status.airplay_track = None;
                        status.airplay_artwork = None;
                    }
                    // Pause/flush are already handled inside the library.
                    _ => {}
                }
            }
        });
        let factory_player = player.clone();
        let sink_factory = move |rate: u32, channels: u8| -> Box<dyn openairplay2::AudioSink> {
            let (bridge_sink, source) = airplay::bridge(rate, channels);
            factory_player.send(player::Command::AirplayStarted { source });
            Box::new(bridge_sink)
        };
        let name = config.airplay.name.clone();
        tokio::spawn(async move {
            if let Err(err) = receiver.run(sink_factory, events_tx).await {
                eprintln!("radiod: airplay receiver stopped: {err} (radio continues)");
            }
        });
        println!("radiod: airplay: advertising as {name:?}");
    }
    let app = Arc::new(server::App {
        status: status.clone(),
        player: player.clone(),
        resolver: Arc::new(pls::resolve),
        web: Arc::new(web::Web::new(args.web_dir.clone())),
    });

    state::spawn_saver(
        config.state_path.clone(),
        status.clone(),
        state::PersistedState {
            volume: Some(config.initial_volume),
        },
    );

    println!(
        "radiod {} listening on http://{}",
        env!("CARGO_PKG_VERSION"),
        config.listen
    );
    tokio::select! {
        () = server::serve(listener, app) => {}
        () = shutdown_signal() => {}
    }

    // Stop the player and wait for it to settle, so the sink is closed
    // (ALSA drained) before the process exits. Waiting on the status
    // instead of joining the thread keeps shutdown independent of any
    // connection task that still holds a Player handle.
    println!("radiod: shutting down");
    player.send(player::Command::Stop);
    let deadline = Instant::now() + Duration::from_secs(5);
    while status.lock().expect("status lock poisoned").state != State::Stopped {
        if Instant::now() >= deadline {
            eprintln!("radiod: player did not stop in time");
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    state::save_now(&config.state_path, &status);
    ExitCode::SUCCESS
}

/// Resolves when the process receives SIGINT (ctrl-c) or SIGTERM (what
/// systemd sends on `systemctl stop`).
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigterm = match signal(SignalKind::terminate()) {
            Ok(sigterm) => sigterm,
            Err(err) => {
                eprintln!("radiod: cannot install SIGTERM handler: {err}");
                std::future::pending().await
            }
        };
        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                if let Err(err) = result {
                    eprintln!("radiod: cannot listen for ctrl-c: {err}");
                    std::future::pending().await
                }
            }
            _ = sigterm.recv() => {}
        }
    }
    #[cfg(not(unix))]
    if let Err(err) = tokio::signal::ctrl_c().await {
        eprintln!("radiod: cannot listen for ctrl-c: {err}");
        std::future::pending().await
    }
}
