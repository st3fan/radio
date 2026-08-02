use serde::Serialize;

use crate::config::Config;

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Playing,
    Paused,
    Stopped,
}

/// Which producer owns the pipeline right now.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AudioSource {
    Radio,
    Airplay,
}

/// Details of the active AirPlay stream.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct AirplayInfo {
    pub rate: u32,
    pub channels: u16,
}

/// Now-playing metadata pushed by the AirPlay sender (DMAP). Each event
/// from the library is a complete statement, so this is replaced
/// wholesale — absent fields mean the sender sent none.
#[derive(Debug, Clone, PartialEq)]
pub struct AirplayTrack {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
}

/// The latest cover art from the AirPlay sender, exactly as sent. The
/// bytes are shared, not copied — `Status` is cloned on every state
/// sample and artwork runs ~180 KB per track.
#[derive(Debug, Clone)]
pub struct AirplayArtwork {
    pub content_type: String,
    pub data: std::sync::Arc<Vec<u8>>,
    /// Monotonic per process; the cache-buster in the artwork URL, so a
    /// track change is a new URL and an unchanged poll is a cache hit.
    pub version: u64,
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
    /// Mixer ceiling health: "ok", "disabled" (null/wav sinks), or
    /// "error: ...". The website surfaces anything that isn't "ok"-ish.
    pub mixer: String,
    /// Which producer owns the pipeline: the radio or an AirPlay session.
    pub source: AudioSource,
    /// Present while an AirPlay stream is active.
    pub airplay: Option<AirplayInfo>,
    /// The AirPlay sender's volume slider as a gain factor in `[0, 1]`,
    /// multiplied into the pipeline gain only while AirPlay is the source.
    /// Session state, not part of the API contract.
    #[serde(skip)]
    pub airplay_gain: f32,
    /// Sender-pushed track metadata; session state for the website,
    /// not part of the API contract.
    #[serde(skip)]
    pub airplay_track: Option<AirplayTrack>,
    /// Sender-pushed cover art, served at /airplay/artwork; session
    /// state for the website, not part of the API contract.
    #[serde(skip)]
    pub airplay_artwork: Option<AirplayArtwork>,
}

impl Status {
    pub fn initial(config: &Config) -> Status {
        Status {
            state: State::Stopped,
            playlist_url: None,
            stream_url: None,
            icy_title: None,
            icy_name: None,
            volume: config.initial_volume,
            muted: false,
            mixer: "disabled".to_string(),
            source: AudioSource::Radio,
            airplay: None,
            airplay_gain: 1.0,
            airplay_track: None,
            airplay_artwork: None,
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
                "volume": 50,
                "muted": false,
                "mixer": "disabled",
                "source": "radio",
                "airplay": null
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
    fn initial_volume_is_taken_verbatim() {
        let config = Config::from_toml("initial_volume = 80").unwrap();
        let status = Status::initial(&config);
        assert_eq!(status.volume, 80);
    }

    #[test]
    fn config_from_toml_used_in_status() {
        let config = Config::default();
        let status = Status::initial(&config);
        assert_eq!(status.state, State::Stopped);
        assert!(!status.muted);
    }
}
