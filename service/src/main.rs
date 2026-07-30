mod config;
mod server;
mod status;
mod volume;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Mutex;

use config::Config;
use status::Status;

const USAGE: &str = "usage: radiod [--config <path>] [-v | --version]";

struct Args {
    config_path: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut args = std::env::args().skip(1);
    let mut config_path = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => {
                let path = args.next().ok_or("--config requires a path")?;
                config_path = Some(PathBuf::from(path));
            }
            "-v" | "--version" => {
                println!("radiod {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument {other:?}\n{USAGE}")),
        }
    }
    Ok(Args { config_path })
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

    let server = match tiny_http::Server::http(config.listen) {
        Ok(server) => server,
        Err(err) => {
            eprintln!("radiod: cannot listen on {}: {err}", config.listen);
            return ExitCode::FAILURE;
        }
    };

    let status = Mutex::new(Status::initial(&config));
    println!(
        "radiod {} listening on http://{}",
        env!("CARGO_PKG_VERSION"),
        config.listen
    );
    server::serve(&server, &status);
    ExitCode::SUCCESS
}
