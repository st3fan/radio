use serde::Serialize;

use crate::config::Config;

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Playing,
    Paused,
    Stopped,
}

#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub state: State,
    pub playlist_url: Option<String>,
    pub stream_url: Option<String>,
    pub icy_title: Option<String>,
    pub icy_name: Option<String>,
    pub volume: u8,
    pub muted: bool,
    pub max_volume: u8,
}

impl Status {
    pub fn initial(config: &Config) -> Status {
        Status {
            state: State::Stopped,
            playlist_url: None,
            stream_url: None,
            icy_title: None,
            icy_name: None,
            // Already effective: config stores the scaled device volume.
            volume: config.initial_volume,
            muted: false,
            max_volume: config.max_volume,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn initial_status_serializes_to_documented_shape() {
        let status = Status::initial(&Config::default());
        let value = serde_json::to_value(&status).unwrap();
        assert_eq!(
            value,
            json!({
                "state": "stopped",
                "playlist_url": null,
                "stream_url": null,
                "icy_title": null,
                "icy_name": null,
                "volume": 25,
                "muted": false,
                "max_volume": 50
            })
        );
    }

    #[test]
    fn states_serialize_lowercase() {
        assert_eq!(
            serde_json::to_value(State::Playing).unwrap(),
            json!("playing")
        );
        assert_eq!(
            serde_json::to_value(State::Paused).unwrap(),
            json!("paused")
        );
        assert_eq!(
            serde_json::to_value(State::Stopped).unwrap(),
            json!("stopped")
        );
    }

    #[test]
    fn initial_volume_respects_max_volume() {
        let config = Config::from_toml("max_volume = 30\ninitial_volume = 80").unwrap();
        let status = Status::initial(&config);
        assert_eq!(status.volume, 24); // 80% of max_volume 30
        assert_eq!(status.max_volume, 30);
    }

    #[test]
    fn config_from_toml_used_in_status() {
        let config = Config::default();
        let status = Status::initial(&config);
        assert_eq!(status.state, State::Stopped);
        assert!(!status.muted);
    }
}
