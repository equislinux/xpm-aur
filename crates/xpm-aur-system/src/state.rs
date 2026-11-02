//! Persistent record of AUR package bases installed by xpm-aur.
//!
//! The pacman/xpm database knows the files and versions, but not which AUR
//! commit produced a package or whether it was requested explicitly. This
//! small sidecar database fills that gap and is intentionally independent from
//! the package manager databases.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StateError {
    #[error("could not access state at {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("could not parse state at {path}: {source}")]
    Json {
        path: String,
        #[source]
        source: serde_json::Error,
    },

    #[error("could not lock state at {path}: {source}")]
    Lock {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct State {
    #[serde(default = "schema_version")]
    pub schema: u32,
    #[serde(default)]
    pub packages: BTreeMap<String, AurRecord>,
}

fn schema_version() -> u32 {
    1
}

impl Default for State {
    fn default() -> Self {
        Self {
            schema: schema_version(),
            packages: BTreeMap::new(),
        }
    }
}

/// One AUR package base, including all split outputs that are installed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AurRecord {
    pub base: String,
    pub version: String,
    pub commit: String,
    pub installed_at: u64,
    pub explicit: bool,
    pub packages: Vec<String>,
}

impl AurRecord {
    pub fn new(
        base: impl Into<String>,
        version: impl Into<String>,
        commit: impl Into<String>,
        packages: Vec<String>,
        explicit: bool,
    ) -> Self {
        Self {
            base: base.into(),
            version: version.into(),
            commit: commit.into(),
            installed_at: now_unix(),
            explicit,
            packages,
        }
    }
}

/// Atomic, lock-protected JSON sidecar.
pub struct StateStore {
    path: PathBuf,
}

impl StateStore {
    /// Default path: `$XDG_STATE_HOME/xpm-aur/state.json`.
    pub fn default_path() -> PathBuf {
        if let Ok(path) = std::env::var("XPM_AUR_STATE") {
            return PathBuf::from(path);
        }
        let base = dirs::state_dir()
            .or_else(|| dirs::home_dir().map(|home| home.join(".local/state")))
            .unwrap_or_else(|| PathBuf::from("/var/lib/xpm-aur"));
        base.join("xpm-aur/state.json")
    }

    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<State, StateError> {
        let _lock = self.lock_shared()?;
        self.load_unlocked()
    }

    pub fn save(&self, state: &State) -> Result<(), StateError> {
        let _lock = self.lock_exclusive()?;
        self.save_unlocked(state)
    }

    pub fn update<F>(&self, mutate: F) -> Result<State, StateError>
    where
        F: FnOnce(&mut State),
    {
        let _lock = self.lock_exclusive()?;
        let mut state = self.load_unlocked()?;
        mutate(&mut state);
        self.save_unlocked(&state)?;
        Ok(state)
    }

    fn load_unlocked(&self) -> Result<State, StateError> {
        match fs::read_to_string(&self.path) {
            Ok(text) => serde_json::from_str(&text).map_err(|source| StateError::Json {
                path: self.path.display().to_string(),
                source,
            }),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(State::default()),
            Err(source) => Err(StateError::Io {
                path: self.path.display().to_string(),
                source,
            }),
        }
    }

    fn save_unlocked(&self, state: &State) -> Result<(), StateError> {
        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent).map_err(|source| StateError::Io {
            path: parent.display().to_string(),
            source,
        })?;
        let tmp = self.path.with_extension("json.tmp");
        let text = serde_json::to_string_pretty(state).expect("state is serializable");
        {
            let mut file = File::create(&tmp).map_err(|source| StateError::Io {
                path: tmp.display().to_string(),
                source,
            })?;
            file.write_all(text.as_bytes())
                .and_then(|_| file.sync_all())
                .map_err(|source| StateError::Io {
                    path: tmp.display().to_string(),
                    source,
                })?;
        }
        fs::rename(&tmp, &self.path).map_err(|source| StateError::Io {
            path: self.path.display().to_string(),
            source,
        })
    }

    fn lock_shared(&self) -> Result<File, StateError> {
        self.lock(false)
    }

    fn lock_exclusive(&self) -> Result<File, StateError> {
        self.lock(true)
    }

    fn lock(&self, exclusive: bool) -> Result<File, StateError> {
        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent).map_err(|source| StateError::Io {
            path: parent.display().to_string(),
            source,
        })?;
        let lock_path = self.path.with_extension("lock");
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|source| StateError::Lock {
                path: lock_path.display().to_string(),
                source,
            })?;
        if exclusive {
            FileExt::lock_exclusive(&file)
        } else {
            FileExt::lock_shared(&file)
        }
        .map_err(|source| StateError::Lock {
            path: lock_path.display().to_string(),
            source,
        })?;
        Ok(file)
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_and_updates_atomically() {
        let dir = std::env::temp_dir().join(format!(
            "xpm-aur-state-test-{}-{}",
            std::process::id(),
            now_unix()
        ));
        let store = StateStore::at(dir.join("state.json"));
        let record = AurRecord::new("demo", "1.0-1", "abc", vec!["demo".into()], true);
        store
            .update(|state| {
                state.packages.insert("demo".into(), record.clone());
            })
            .unwrap();

        let loaded = store.load().unwrap();
        assert_eq!(loaded.packages["demo"], record);

        store
            .update(|state| {
                state.packages.remove("demo");
            })
            .unwrap();
        assert!(store.load().unwrap().packages.is_empty());
        let _ = fs::remove_dir_all(dir);
    }
}
