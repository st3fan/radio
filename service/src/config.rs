use std::fmt;
use std::net::SocketAddr;
use std::path::Path;

use serde::Deserialize;

pub const DEFAULT_CONFIG_PATH: &str = "/etc/radio/config.toml";

const DEFAULT_LISTEN: &str = "127.0.0.1:8080";
const DEFAULT_AUDIO_DEVICE: &str = "plughw:1,0";
const DEFAULT_INITIAL_VOLUME: u8 = 50;

/// The hardware output ceiling radiod owns (milestone 10): the speaker
/// protection lives in the ALSA mixer, not in a software cap.
#[derive(Debug, Clone, PartialEq)]
pub struct MixerConfig {
    /// Simple mixer control name, e.g. "PCM" or "Speaker".
    pub control: String,
    /// Mixer device ("hw:N"); derived from `audio_device` when absent.
    pub device: Option<String>,
    pub ceiling: MixerCeiling,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MixerCeiling {
    /// Preferred: an absolute level in dB (0 = full scale, negative = quieter).
    Db(f32),
    /// Fallback for controls without dB info: percent of the raw range,
    /// which may be nonlinear.
    Percent(u8),
}

/// The embedded AirPlay receiver (milestone 11).
#[derive(Debug, Clone, PartialEq)]
pub struct AirplayConfig {
    pub enabled: bool,
    /// The name shown in Apple devices' AirPlay pickers.
    pub name: String,
    pub port: u16,
    /// When an AirPlay session ends, resume the preempted station.
    pub resume_radio: bool,
    /// The receiver identity (keypair): state, not config — senders
    /// remember the receiver by it, so it must survive upgrades.
    pub identity_path: std::path::PathBuf,
}

impl Default for AirplayConfig {
    fn default() -> Self {
        AirplayConfig {
            // AirPlay needs Avahi over D-Bus: on by default on Linux only.
            enabled: cfg!(target_os = "linux"),
            name: "Radio".to_string(),
            port: 7000,
            resume_radio: true,
            identity_path: std::path::PathBuf::from("/var/lib/radiod/airplay-identity"),
        }
    }
}

/// Loaded configuration. `initial_volume` is a plain 0-100 volume; the
/// speaker-protecting ceiling is the `[mixer]` section, required when
/// playing through ALSA.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub listen: SocketAddr,
    pub audio_device: String,
    pub initial_volume: u8,
    pub mixer: Option<MixerConfig>,
    pub airplay: AirplayConfig,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            listen: DEFAULT_LISTEN.parse().expect("default listen address"),
            audio_device: DEFAULT_AUDIO_DEVICE.to_string(),
            initial_volume: DEFAULT_INITIAL_VOLUME,
            mixer: None,
            airplay: AirplayConfig::default(),
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
/// `max_volume` is kept only to give its removal a named migration error.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    listen: Option<String>,
    audio_device: Option<String>,
    max_volume: Option<u8>,
    initial_volume: Option<u8>,
    mixer: Option<RawMixerConfig>,
    airplay: Option<RawAirplayConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMixerConfig {
    control: Option<String>,
    device: Option<String>,
    ceiling_db: Option<f32>,
    ceiling_percent: Option<u8>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAirplayConfig {
    enabled: Option<bool>,
    name: Option<String>,
    port: Option<u16>,
    resume_radio: Option<bool>,
    identity_path: Option<String>,
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

        // The software cap is gone; refuse the old key with a pointer to
        // its replacement instead of a generic unknown-field error.
        if raw.max_volume.is_some() {
            return Err(ConfigError::Invalid(
                "max_volume was replaced by the [mixer] section: radiod now \
                 sets a hardware ceiling on an ALSA mixer control instead of \
                 capping in software. See config.toml.example (control = \
                 \"...\" plus ceiling_db or ceiling_percent)"
                    .to_string(),
            ));
        }

        let initial_volume = raw.initial_volume.unwrap_or(DEFAULT_INITIAL_VOLUME);
        if initial_volume > 100 {
            return Err(ConfigError::Invalid(format!(
                "initial_volume {initial_volume} is out of range (0-100)"
            )));
        }

        let mixer = match raw.mixer {
            None => None,
            Some(raw_mixer) => Some(Config::validate_mixer(raw_mixer)?),
        };

        let airplay_defaults = AirplayConfig::default();
        let airplay = match raw.airplay {
            None => airplay_defaults,
            Some(raw_airplay) => {
                let name = raw_airplay.name.unwrap_or(airplay_defaults.name);
                if name.is_empty() {
                    return Err(ConfigError::Invalid(
                        "[airplay] name must not be empty".to_string(),
                    ));
                }
                AirplayConfig {
                    enabled: raw_airplay.enabled.unwrap_or(airplay_defaults.enabled),
                    name,
                    port: raw_airplay.port.unwrap_or(airplay_defaults.port),
                    resume_radio: raw_airplay
                        .resume_radio
                        .unwrap_or(airplay_defaults.resume_radio),
                    identity_path: raw_airplay
                        .identity_path
                        .map(std::path::PathBuf::from)
                        .unwrap_or(airplay_defaults.identity_path),
                }
            }
        };

        Ok(Config {
            listen,
            audio_device: raw.audio_device.unwrap_or(defaults.audio_device),
            initial_volume,
            mixer,
            airplay,
        })
    }

    fn validate_mixer(raw: RawMixerConfig) -> Result<MixerConfig, ConfigError> {
        let Some(control) = raw.control else {
            return Err(ConfigError::Invalid(
                "[mixer] needs control = \"<element>\" (discover with \
                 `amixer -c<card> scontrols`)"
                    .to_string(),
            ));
        };
        let ceiling = match (raw.ceiling_db, raw.ceiling_percent) {
            (Some(db), None) => {
                if !db.is_finite() {
                    return Err(ConfigError::Invalid(format!(
                        "[mixer] ceiling_db {db} is not a finite number"
                    )));
                }
                MixerCeiling::Db(db)
            }
            (None, Some(percent)) => {
                if percent > 100 {
                    return Err(ConfigError::Invalid(format!(
                        "[mixer] ceiling_percent {percent} is out of range (0-100)"
                    )));
                }
                MixerCeiling::Percent(percent)
            }
            (None, None) | (Some(_), Some(_)) => {
                return Err(ConfigError::Invalid(
                    "[mixer] needs exactly one of ceiling_db (preferred) or \
                     ceiling_percent"
                        .to_string(),
                ));
            }
        };
        Ok(MixerConfig {
            control,
            device: raw.device,
            ceiling,
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
        assert_eq!(config.initial_volume, 50);
        assert_eq!(config.mixer, None);
    }

    #[test]
    fn full_file_parses() {
        let config = Config::from_toml(
            r#"
            listen = "127.0.0.1:9000"
            audio_device = "plughw:2,0"
            initial_volume = 10

            [mixer]
            control = "PCM"
            device = "hw:2"
            ceiling_db = -12.5
            "#,
        )
        .unwrap();
        assert_eq!(config.listen, "127.0.0.1:9000".parse().unwrap());
        assert_eq!(config.audio_device, "plughw:2,0");
        assert_eq!(config.initial_volume, 10);
        let mixer = config.mixer.unwrap();
        assert_eq!(mixer.control, "PCM");
        assert_eq!(mixer.device.as_deref(), Some("hw:2"));
        assert_eq!(mixer.ceiling, MixerCeiling::Db(-12.5));
    }

    #[test]
    fn mixer_percent_ceiling_parses() {
        let config =
            Config::from_toml("[mixer]\ncontrol = \"Speaker\"\nceiling_percent = 40").unwrap();
        let mixer = config.mixer.unwrap();
        assert_eq!(mixer.ceiling, MixerCeiling::Percent(40));
        assert_eq!(mixer.device, None);
    }

    #[test]
    fn old_max_volume_key_names_the_migration() {
        let err = Config::from_toml("max_volume = 50").unwrap_err();
        let ConfigError::Invalid(message) = err else {
            panic!("expected Invalid, got {err:?}");
        };
        assert!(message.contains("[mixer]"), "message: {message}");
        assert!(message.contains("max_volume"), "message: {message}");
    }

    #[test]
    fn mixer_without_control_is_rejected() {
        assert!(matches!(
            Config::from_toml("[mixer]\nceiling_db = -10"),
            Err(ConfigError::Invalid(_))
        ));
    }

    #[test]
    fn mixer_needs_exactly_one_ceiling() {
        for toml in [
            "[mixer]\ncontrol = \"PCM\"",
            "[mixer]\ncontrol = \"PCM\"\nceiling_db = -10\nceiling_percent = 40",
        ] {
            assert!(
                matches!(Config::from_toml(toml), Err(ConfigError::Invalid(_))),
                "accepted: {toml}"
            );
        }
    }

    #[test]
    fn mixer_percent_above_100_is_rejected() {
        assert!(matches!(
            Config::from_toml("[mixer]\ncontrol = \"PCM\"\nceiling_percent = 101"),
            Err(ConfigError::Invalid(_))
        ));
    }

    #[test]
    fn mixer_non_finite_db_is_rejected() {
        assert!(matches!(
            Config::from_toml("[mixer]\ncontrol = \"PCM\"\nceiling_db = inf"),
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
