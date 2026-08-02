//! Runtime settings persistence: the growable state file.
//!
//! Settings changed at runtime (today: the volume) are state, not config —
//! they live in the systemd `StateDirectory` next to the AirPlay identity
//! and survive restarts, reboots and reinstalls. State is convenience, not
//! safety: a missing, corrupt or unwritable file logs one clear line and
//! the daemon runs on config defaults; the mixer ceiling caps output in
//! hardware regardless.
//!
//! Saving is an observer, not touchpoint sprawl: a task samples the shared
//! status and atomically rewrites the file only when the persisted fields
//! changed, so future fields are one struct member away and no volume
//! mutation site needs to know about persistence.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::status::Status;

/// How often the saver samples the status. A hard power cut loses at most
/// this much knob-turning.
const SAVE_INTERVAL: Duration = Duration::from_secs(2);

/// What gets persisted. Every field is optional so files from older or
/// newer versions load cleanly (unknown fields are tolerated for the same
/// reason — the file is machine-written, not operator-written config).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PersistedState {
    pub volume: Option<u8>,
}

impl PersistedState {
    fn snapshot(status: &Mutex<Status>) -> PersistedState {
        PersistedState {
            volume: Some(status.lock().expect("status lock poisoned").volume),
        }
    }
}

/// Loads the state file; any failure is logged and yields `None` — the
/// caller falls back to config defaults. Values are sanitized (the file is
/// hand-editable even if machine-written).
pub fn load(path: &Path) -> Option<PersistedState> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
        Err(err) => {
            eprintln!(
                "radiod: state: cannot read {} ({err}); using config defaults",
                path.display()
            );
            return None;
        }
    };
    match toml::from_str::<PersistedState>(&contents) {
        Ok(mut state) => {
            state.volume = state.volume.map(|v| v.min(100));
            Some(state)
        }
        Err(err) => {
            eprintln!(
                "radiod: state: cannot parse {} ({err}); using config defaults",
                path.display()
            );
            None
        }
    }
}

/// Atomically writes the state file (temp file + rename in the same
/// directory), creating the parent directory if needed.
pub fn save(path: &Path, state: &PersistedState) -> std::io::Result<()> {
    let contents = toml::to_string(state)
        .map_err(|err| std::io::Error::other(format!("cannot serialize state: {err}")))?;
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)
}

/// Saves when the state differs from the last written one; returns whether
/// a write happened. Write failures are reported once, not every interval.
fn save_if_changed(
    path: &Path,
    current: PersistedState,
    last: &mut Option<PersistedState>,
    warned: &mut bool,
) -> bool {
    if last.as_ref() == Some(&current) {
        return false;
    }
    match save(path, &current) {
        Ok(()) => {
            *last = Some(current);
            *warned = false;
            true
        }
        Err(err) => {
            if !*warned {
                eprintln!(
                    "radiod: state: cannot write {} ({err}); settings will not persist",
                    path.display()
                );
                *warned = true;
            }
            false
        }
    }
}

/// The saver task: samples the status every couple of seconds and rewrites
/// the file on change. `initial` seeds the comparison so startup does not
/// rewrite a file it just loaded.
pub fn spawn_saver(path: PathBuf, status: Arc<Mutex<Status>>, initial: PersistedState) {
    tokio::spawn(async move {
        let mut last = Some(initial);
        let mut warned = false;
        loop {
            tokio::time::sleep(SAVE_INTERVAL).await;
            let current = PersistedState::snapshot(&status);
            save_if_changed(&path, current, &mut last, &mut warned);
        }
    });
}

/// The best-effort final save on graceful shutdown.
pub fn save_now(path: &Path, status: &Mutex<Status>) {
    let current = PersistedState::snapshot(status);
    if let Err(err) = save(path, &current) {
        eprintln!(
            "radiod: state: final save to {} failed: {err}",
            path.display()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_state_path(name: &str) -> PathBuf {
        std::env::temp_dir()
            .join("radiod-state-tests")
            .join(format!("{name}-{}", std::process::id()))
            .join("state.toml")
    }

    #[test]
    fn save_and_load_round_trip() {
        let path = temp_state_path("round-trip");
        let state = PersistedState { volume: Some(35) };
        save(&path, &state).unwrap();
        assert_eq!(load(&path), Some(state));
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn missing_file_is_none() {
        assert_eq!(load(Path::new("/nonexistent/radiod/state.toml")), None);
    }

    #[test]
    fn corrupt_file_falls_back_to_none() {
        let path = temp_state_path("corrupt");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "volume = \"eleven\"").unwrap();
        assert_eq!(load(&path), None);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn out_of_range_volume_is_clamped() {
        let path = temp_state_path("clamp");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "volume = 250").unwrap();
        assert_eq!(load(&path).unwrap().volume, Some(100));
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn fields_from_other_versions_are_tolerated() {
        let path = temp_state_path("future");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "volume = 20\nlast_station = \"defcon\"").unwrap();
        assert_eq!(load(&path).unwrap().volume, Some(20));
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn saver_writes_only_on_change() {
        let path = temp_state_path("on-change");
        let mut last = None;
        let mut warned = false;
        let state = PersistedState { volume: Some(40) };
        assert!(save_if_changed(&path, state, &mut last, &mut warned));
        assert!(!save_if_changed(&path, state, &mut last, &mut warned));
        let louder = PersistedState { volume: Some(50) };
        assert!(save_if_changed(&path, louder, &mut last, &mut warned));
        assert_eq!(load(&path).unwrap().volume, Some(50));
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn unwritable_path_warns_once_and_never_persists() {
        // The parent is a *file*, so the path is unwritable even as root —
        // CI containers run as root, where absolute nonexistent paths are
        // happily creatable (found by the v0.4.0 release run).
        let blocker =
            std::env::temp_dir().join(format!("radiod-state-blocker-{}", std::process::id()));
        std::fs::write(&blocker, "not a directory").unwrap();
        let path = blocker.join("state.toml");
        let mut last = None;
        let mut warned = false;
        let state = PersistedState { volume: Some(40) };
        assert!(!save_if_changed(&path, state, &mut last, &mut warned));
        assert!(warned);
        assert_eq!(last, None, "a failed write must not update the baseline");
        std::fs::remove_file(&blocker).ok();
    }
}
