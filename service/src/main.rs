mod config;
mod icy;
mod mixer;
mod pipeline;
mod player;
mod pls;
mod server;
mod sink;
mod source;
mod status;
mod volume;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use std::time::{Duration, Instant};

use config::Config;
use pipeline::FfmpegSource;
use sink::{AudioSink, NullSink, WavSink};
use source::{Source, SourceError};
use status::{State, Status};

const USAGE: &str =
    "usage: radiod [--config <path>] [--sink alsa|null|wav:<path>] [-v | --version]";

#[cfg(target_os = "linux")]
const DEFAULT_SINK: &str = "alsa";
#[cfg(not(target_os = "linux"))]
const DEFAULT_SINK: &str = "null";

struct Args {
    config_path: Option<PathBuf>,
    sink: Option<String>,
}

fn parse_args() -> Result<Args, String> {
    let mut args = std::env::args().skip(1);
    let mut config_path = None;
    let mut sink = None;
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
            "-v" | "--version" => {
                println!("radiod {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument {other:?}\n{USAGE}")),
        }
    }
    Ok(Args { config_path, sink })
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

    let config = match load_config(&args) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("radiod: {err}");
            return ExitCode::FAILURE;
        }
    };

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
    let player = player::spawn(status.clone(), sink, Box::new(make_source));
    let app = Arc::new(server::App {
        status: status.clone(),
        player: player.clone(),
        resolver: Arc::new(pls::resolve),
    });

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
