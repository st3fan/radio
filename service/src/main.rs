mod config;
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

use config::Config;
use sink::{AudioSink, NullSink, WavSink};
use source::{SineSource, Source, SourceError};
use status::Status;

const USAGE: &str = "usage: radiod [--config <path>] [--sink null|wav:<path>] [-v | --version]";

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

/// Builds the sink from --sink. The default is null until the ALSA sink
/// lands in milestone 2 phase 3 (which will make alsa the Linux default).
fn make_sink(args: &Args) -> Result<Box<dyn AudioSink>, String> {
    match args.sink.as_deref() {
        None | Some("null") => Ok(Box::new(NullSink)),
        Some(value) => match value.split_once(':') {
            Some(("wav", path)) if !path.is_empty() => {
                Ok(Box::new(WavSink::new(PathBuf::from(path))))
            }
            _ => Err(format!("unknown sink {value:?}\n{USAGE}")),
        },
    }
}

/// Phase 1: every stream URL "plays" the built-in test tone. Phase 2
/// replaces this with the FFmpeg pipeline.
fn make_source(_stream_url: &str) -> Result<Box<dyn Source>, SourceError> {
    Ok(Box::new(SineSource::new(true)))
}

fn main() -> ExitCode {
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

    let sink = match make_sink(&args) {
        Ok(sink) => sink,
        Err(err) => {
            eprintln!("radiod: {err}");
            return ExitCode::from(2);
        }
    };

    let server = match tiny_http::Server::http(config.listen) {
        Ok(server) => server,
        Err(err) => {
            eprintln!("radiod: cannot listen on {}: {err}", config.listen);
            return ExitCode::FAILURE;
        }
    };

    let status = Arc::new(Mutex::new(Status::initial(&config)));
    let player = player::spawn(status.clone(), sink, Box::new(make_source));
    let app = server::App {
        status,
        player,
        resolver: Box::new(pls::resolve),
    };

    println!(
        "radiod {} listening on http://{}",
        env!("CARGO_PKG_VERSION"),
        config.listen
    );
    server::serve(&server, &app);
    ExitCode::SUCCESS
}
