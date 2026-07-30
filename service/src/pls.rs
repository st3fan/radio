//! Fetching and parsing `.pls` playlists (the 5-line INI format SomaFM and
//! most icecast stations publish). libavformat has no pls demuxer, so we
//! resolve the stream URL ourselves before handing it to the player.

use std::fmt;

#[derive(Debug)]
pub enum PlsError {
    Fetch(String),
    Parse(String),
}

impl fmt::Display for PlsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlsError::Fetch(msg) => write!(f, "cannot fetch playlist: {msg}"),
            PlsError::Parse(msg) => write!(f, "cannot parse playlist: {msg}"),
        }
    }
}

impl std::error::Error for PlsError {}

/// Fetches a playlist URL and returns the first stream URL in it.
pub fn resolve(playlist_url: &str) -> Result<String, PlsError> {
    let body = ureq::get(playlist_url)
        .call()
        .map_err(|err| PlsError::Fetch(err.to_string()))?
        .body_mut()
        .read_to_string()
        .map_err(|err| PlsError::Fetch(err.to_string()))?;
    parse(&body)
}

/// Returns the URL of the lowest-numbered `FileN` entry. Keys are matched
/// case-insensitively; SomaFM emits `File1=...` but other stations vary.
pub fn parse(contents: &str) -> Result<String, PlsError> {
    let mut best: Option<(u32, &str)> = None;
    for line in contents.lines() {
        let line = line.trim();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let Some(number) = key.strip_prefix("file") else {
            continue;
        };
        let Ok(number) = number.parse::<u32>() else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        if best.is_none_or(|(n, _)| number < n) {
            best = Some((number, value));
        }
    }
    match best {
        Some((_, url)) => Ok(url.to_string()),
        None => Err(PlsError::Parse("no File entries found".to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOMAFM: &str = "[playlist]\n\
        numberofentries=3\n\
        File1=https://ice2.somafm.com/defcon-128-mp3\n\
        Title1=SomaFM - DEF CON Radio (#1): Music for Hacking.\n\
        Length1=-1\n\
        File2=https://ice4.somafm.com/defcon-128-mp3\n\
        Title2=SomaFM - DEF CON Radio (#2): Music for Hacking.\n\
        Length2=-1\n\
        File3=https://ice1.somafm.com/defcon-128-mp3\n\
        Title3=SomaFM - DEF CON Radio (#3): Music for Hacking.\n\
        Length3=-1\n\
        Version=2\n";

    #[test]
    fn parses_somafm_playlist() {
        assert_eq!(
            parse(SOMAFM).unwrap(),
            "https://ice2.somafm.com/defcon-128-mp3"
        );
    }

    #[test]
    fn lowest_numbered_entry_wins_regardless_of_order() {
        let contents = "File2=https://example.com/second\nFile1=https://example.com/first\n";
        assert_eq!(parse(contents).unwrap(), "https://example.com/first");
    }

    #[test]
    fn keys_are_case_insensitive() {
        assert_eq!(
            parse("FILE1=https://example.com/stream").unwrap(),
            "https://example.com/stream"
        );
        assert_eq!(
            parse("file1=https://example.com/stream").unwrap(),
            "https://example.com/stream"
        );
    }

    #[test]
    fn whitespace_is_trimmed() {
        assert_eq!(
            parse("  File1 = https://example.com/stream  \n").unwrap(),
            "https://example.com/stream"
        );
    }

    #[test]
    fn missing_file_entries_is_an_error() {
        assert!(matches!(
            parse("[playlist]\nnumberofentries=0\n"),
            Err(PlsError::Parse(_))
        ));
        assert!(matches!(parse(""), Err(PlsError::Parse(_))));
    }

    #[test]
    fn empty_file_value_is_ignored() {
        assert!(matches!(parse("File1=\n"), Err(PlsError::Parse(_))));
    }

    #[test]
    fn unrelated_keys_are_ignored() {
        let contents = "Title1=Not a url\nFilename=nope\nFile1=https://example.com/s\n";
        assert_eq!(parse(contents).unwrap(), "https://example.com/s");
    }
}
