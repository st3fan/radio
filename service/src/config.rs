use std::fmt;
use std::net::SocketAddr;
use std::path::Path;

use serde::Deserialize;

use crate::volume;

pub const DEFAULT_CONFIG_PATH: &str = "/etc/radio/config.toml";

const DEFAULT_LISTEN: &str = "127.0.0.1:8080";
const DEFAULT_AUDIO_DEVICE: &str = "plughw:1,0";
const DEFAULT_MAX_VOLUME: u8 = 50;
const DEFAULT_INITIAL_VOLUME: u8 = 25;

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub listen: SocketAddr,
    pub audio_device: String,
    pub max_volume: u8,
    pub initial_volume: u8,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            listen: DEFAULT_LISTEN.parse().expect("default listen address"),
            audio_device: DEFAULT_AUDIO_DEVICE.to_string(),
            max_volume: DEFAULT_MAX_VOLUME,
            initial_volume: DEFAULT_INITIAL_VOLUME,
        }
    }
}

#[derive(Debug)]
pub enum ConfigError {
    Io(std::io::Error),
    Toml(toml::de::Error),
    Invalid(String),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Io(err) => write!(f, "cannot read config file: {err}"),
            ConfigError::Toml(err) => write!(f, "cannot parse config file: {err}"),
            ConfigError::Invalid(msg) => write!(f, "invalid config: {msg}"),
        }
    }
}

impl std::error::Error for ConfigError {}

/// The raw file contents; every field is optional and unknown keys are
/// rejected so a typo cannot silently leave a default in place.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    listen: Option<String>,
    audio_device: Option<String>,
    max_volume: Option<u8>,
    initial_volume: Option<u8>,
}

impl Config {
    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        let contents = std::fs::read_to_string(path).map_err(ConfigError::Io)?;
        Config::from_toml(&contents)
    }

    pub fn from_toml(contents: &str) -> Result<Config, ConfigError> {
        let raw: RawConfig = toml::from_str(contents).map_err(ConfigError::Toml)?;
        let defaults = Config::default();

        let listen: SocketAddr = match raw.listen {
            Some(s) => s.parse().map_err(|_| {
                ConfigError::Invalid(format!(
                    "listen address {s:?} is not a valid socket address"
                ))
            })?,
            None => defaults.listen,
        };
        if !listen.ip().is_loopback() {
            return Err(ConfigError::Invalid(format!(
                "listen address {listen} is not a loopback address; the API is loopback-only by design"
            )));
        }

        let max_volume = raw.max_volume.unwrap_or(defaults.max_volume);
        if max_volume > 100 {
            return Err(ConfigError::Invalid(format!(
                "max_volume {max_volume} is out of range (0-100)"
            )));
        }

        let initial_volume = raw.initial_volume.unwrap_or(defaults.initial_volume);
        if initial_volume > 100 {
            return Err(ConfigError::Invalid(format!(
                "initial_volume {initial_volume} is out of range (0-100)"
            )));
        }

        Ok(Config {
            listen,
            audio_device: raw.audio_device.unwrap_or(defaults.audio_device),
            max_volume,
            initial_volume: volume::effective_volume(initial_volume, max_volume),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_yields_defaults() {
        let config = Config::from_toml("").unwrap();
        assert_eq!(config, Config::default());
        assert_eq!(config.listen, "127.0.0.1:8080".parse().unwrap());
        assert_eq!(config.audio_device, "plughw:1,0");
        assert_eq!(config.max_volume, 50);
        assert_eq!(config.initial_volume, 25);
    }

    #[test]
    fn full_file_parses() {
        let config = Config::from_toml(
            r#"
            listen = "127.0.0.1:9000"
            audio_device = "plughw:2,0"
            max_volume = 40
            initial_volume = 10
            "#,
        )
        .unwrap();
        assert_eq!(config.listen, "127.0.0.1:9000".parse().unwrap());
        assert_eq!(config.audio_device, "plughw:2,0");
        assert_eq!(config.max_volume, 40);
        assert_eq!(config.initial_volume, 10);
    }

    #[test]
    fn partial_file_keeps_other_defaults() {
        let config = Config::from_toml("max_volume = 30").unwrap();
        assert_eq!(config.max_volume, 30);
        assert_eq!(config.listen, Config::default().listen);
        assert_eq!(config.audio_device, Config::default().audio_device);
    }

    #[test]
    fn initial_volume_is_clamped_to_max_volume() {
        let config = Config::from_toml("max_volume = 30\ninitial_volume = 80").unwrap();
        assert_eq!(config.initial_volume, 30);
    }

    #[test]
    fn max_volume_above_100_is_rejected() {
        assert!(matches!(
            Config::from_toml("max_volume = 101"),
            Err(ConfigError::Invalid(_))
        ));
    }

    #[test]
    fn initial_volume_above_100_is_rejected() {
        assert!(matches!(
            Config::from_toml("initial_volume = 101"),
            Err(ConfigError::Invalid(_))
        ));
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(matches!(
            Config::from_toml("max_volum = 90"),
            Err(ConfigError::Toml(_))
        ));
    }

    #[test]
    fn non_loopback_listen_is_rejected() {
        assert!(matches!(
            Config::from_toml(r#"listen = "0.0.0.0:8080""#),
            Err(ConfigError::Invalid(_))
        ));
        assert!(matches!(
            Config::from_toml(r#"listen = "192.168.1.10:8080""#),
            Err(ConfigError::Invalid(_))
        ));
    }

    #[test]
    fn ipv6_loopback_is_accepted() {
        let config = Config::from_toml(r#"listen = "[::1]:8080""#).unwrap();
        assert!(config.listen.ip().is_loopback());
    }

    #[test]
    fn malformed_listen_is_rejected() {
        assert!(matches!(
            Config::from_toml(r#"listen = "not an address""#),
            Err(ConfigError::Invalid(_))
        ));
    }

    #[test]
    fn malformed_toml_is_rejected() {
        assert!(matches!(
            Config::from_toml("max_volume = "),
            Err(ConfigError::Toml(_))
        ));
    }

    #[test]
    fn missing_file_is_an_io_error() {
        assert!(matches!(
            Config::load(Path::new("/nonexistent/radio/config.toml")),
            Err(ConfigError::Io(_))
        ));
    }
}
