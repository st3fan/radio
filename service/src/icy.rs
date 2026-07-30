//! Parsing of ICY (icecast/shoutcast) metadata strings.
//!
//! The metadata packet looks like `StreamTitle='Artist - Song';StreamUrl='';`
//! and the response headers like `icy-name:SomaFM ...\r\nicy-genre:...`.

/// The metadata a source can report. `None` fields mean "not provided by
/// the stream", not "unchanged".
#[derive(Debug, Clone, PartialEq)]
pub struct IcyMetadata {
    pub name: Option<String>,
    pub title: Option<String>,
}

/// Extracts the StreamTitle value from an ICY metadata packet. Empty titles
/// (some streams send them between songs) are treated as absent.
pub fn parse_stream_title(packet: &str) -> Option<String> {
    let start = packet.find("StreamTitle='")? + "StreamTitle='".len();
    // The value ends at the next "';". Apostrophes inside a title are fine
    // as long as they are not immediately followed by a semicolon — the
    // ICY format has no escaping, so this is the best any client can do
    // (mpv does the same).
    let end = packet[start..].find("';")? + start;
    let title = packet[start..end].trim();
    if title.is_empty() {
        None
    } else {
        Some(title.to_string())
    }
}

/// Extracts the station name from the raw ICY response headers.
pub fn parse_icy_name(headers: &str) -> Option<String> {
    for line in headers.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if key.trim().eq_ignore_ascii_case("icy-name") {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_standard_stream_title() {
        assert_eq!(
            parse_stream_title("StreamTitle='Nightmares on Wax - Les Nuits';StreamUrl='';"),
            Some("Nightmares on Wax - Les Nuits".to_string())
        );
    }

    #[test]
    fn parses_title_without_trailing_fields() {
        assert_eq!(
            parse_stream_title("StreamTitle='Solo';"),
            Some("Solo".to_string())
        );
    }

    #[test]
    fn title_may_contain_apostrophes_and_semicolons() {
        assert_eq!(
            parse_stream_title("StreamTitle='Let's Go; Right Now';StreamUrl='';"),
            Some("Let's Go; Right Now".to_string())
        );
    }

    #[test]
    fn empty_title_is_absent() {
        assert_eq!(parse_stream_title("StreamTitle='';StreamUrl='';"), None);
        assert_eq!(parse_stream_title("StreamTitle='  ';"), None);
    }

    #[test]
    fn missing_title_is_absent() {
        assert_eq!(parse_stream_title(""), None);
        assert_eq!(parse_stream_title("StreamUrl='x';"), None);
        assert_eq!(parse_stream_title("StreamTitle='unterminated"), None);
    }

    #[test]
    fn parses_icy_name_from_headers() {
        let headers =
            "icy-br:128\r\nicy-genre:ambient\r\nicy-name:SomaFM - DEF CON Radio\r\nicy-pub:0\r\n";
        assert_eq!(
            parse_icy_name(headers),
            Some("SomaFM - DEF CON Radio".to_string())
        );
    }

    #[test]
    fn icy_name_key_is_case_insensitive() {
        assert_eq!(
            parse_icy_name("ICY-Name: Groove Salad\r\n"),
            Some("Groove Salad".to_string())
        );
    }

    #[test]
    fn missing_or_empty_icy_name_is_absent() {
        assert_eq!(parse_icy_name(""), None);
        assert_eq!(parse_icy_name("icy-genre:ambient\r\n"), None);
        assert_eq!(parse_icy_name("icy-name:\r\n"), None);
    }
}
