use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;
use serde_json::{Map, Value};

use crate::error::{fail, Code, Error, Result};
use crate::fsx::atomic::{read_to_string_or_none, write_json_atomic};

use super::sanitize::sanitize;
use super::schema::{migrate, Settings, SCHEMA_VERSION};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum LoadStatus {
    /// No settings file yet - a first run.
    Fresh,
    /// Read cleanly at the current schema version.
    Loaded,
    /// Read at an older schema and migrated forward.
    Migrated,
    /// The primary file was unusable; the previous good copy was used instead.
    RecoveredFromBackup,
    /// Both copies were unusable; the wreckage was set aside, not deleted.
    Quarantined,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Health {
    pub status: LoadStatus,
    /// Where a corrupt file was moved to, so the UI can point a user at it.
    pub quarantined_to: Option<String>,
    /// Last write failure. Some(..) means memory is ahead of disk.
    pub write_error: Option<WriteError>,
}

struct State {
    settings: Settings,
    health: Health,
}

/// The one owner of the settings file.
///
/// Reads are cheap clones of an in-memory copy. Writes take the lock, so
/// concurrent callers each doing `scans.insert(key, result)` cannot lose one
/// another's work - which is exactly what a read-modify-write per handler does.
pub struct SettingsStore {
    file: PathBuf,
    state: Mutex<State>,
}

/// Parse and migrate, or fail loudly.
///
/// Returning a blank default for anything unexpected is what turns a stray
/// byte into "all your settings are gone", so this is deliberately noisy and
/// the caller decides the recovery policy.
fn parse(text: &str) -> Result<(Settings, bool)> {
    let raw: Value = serde_json::from_str(text)
        .map_err(|error| Error::new(Code::StateCorrupt, format!("not valid JSON: {error}")))?;
    let Some(object) = raw.as_object() else {
        return fail(Code::StateCorrupt, "settings file is not a JSON object");
    };

    // An absent `schema` field means the original upstream layout: version 1.
    //
    // Saturating rather than casting: a hand-edited file claiming schema
    // 2^32 + 1 must read as "from the future" and be refused, not wrap around
    // to a version this build thinks it can migrate.
    let found = object
        .get("schema")
        .and_then(Value::as_u64)
        .map_or(1, |value| u32::try_from(value).unwrap_or(u32::MAX));

    if found > SCHEMA_VERSION {
        // A newer build wrote this. Migrating backwards would silently discard
        // whatever it added, so refuse and leave the file untouched.
        return fail(
            Code::StateVersionAhead,
            format!(
                "written by a newer version (schema {found}, this build supports {SCHEMA_VERSION})"
            ),
        );
    }

    let mut current: Map<String, Value> = object.clone();
    let mut version = found;
    while version < SCHEMA_VERSION {
        let Some(stepped) = migrate(version, &current) else {
            return fail(
                Code::StateCorrupt,
                format!("no migration path from schema {version}"),
            );
        };
        let next = stepped
            .get("schema")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or_else(|| version.saturating_add(1));
        if next <= version {
            return fail(Code::StateCorrupt, "migration did not advance the schema");
        }
        current = stepped;
        version = next;
    }

    Ok((sanitize(&Value::Object(current)), found != SCHEMA_VERSION))
}

impl SettingsStore {
    fn backup_path(file: &Path) -> PathBuf {
        let mut name = file.as_os_str().to_owned();
        name.push(".bak");
        PathBuf::from(name)
    }

    pub fn open(file: impl Into<PathBuf>) -> Result<Self> {
        let file = file.into();

        let primary = match read_to_string_or_none(&file) {
            Ok(text) => text,
            // Unreadable-but-present is a real failure, not an absence, and it
            // is handled below like any other unusable primary.
            Err(_) => Some(String::new()),
        };

        let Some(primary) = primary else {
            return Ok(Self::with(
                file,
                Settings::default(),
                Health {
                    status: LoadStatus::Fresh,
                    quarantined_to: None,
                    write_error: None,
                },
            ));
        };

        match parse(&primary) {
            Ok((settings, migrated)) => Ok(Self::with(
                file,
                settings,
                Health {
                    status: if migrated {
                        LoadStatus::Migrated
                    } else {
                        LoadStatus::Loaded
                    },
                    quarantined_to: None,
                    write_error: None,
                },
            )),
            Err(error) if error.code == Code::StateVersionAhead => {
                // Not corruption: the file is fine and belongs to a newer
                // build. Never quarantine it; let the caller show the mismatch.
                Err(error)
            }
            Err(_) => {
                if let Ok(Some(backup)) = read_to_string_or_none(&Self::backup_path(&file)) {
                    if let Ok((settings, _)) = parse(&backup) {
                        return Ok(Self::with(
                            file,
                            settings,
                            Health {
                                status: LoadStatus::RecoveredFromBackup,
                                quarantined_to: None,
                                write_error: None,
                            },
                        ));
                    }
                }

                // Set the wreckage aside under a timestamped name. It may be
                // the only copy of a hand-curated library, so it is never
                // deleted or overwritten.
                let quarantine = Self::quarantine_path(&file);
                let moved = fs::rename(&file, &quarantine).is_ok();
                Ok(Self::with(
                    file,
                    Settings::default(),
                    Health {
                        status: LoadStatus::Quarantined,
                        quarantined_to: moved.then(|| quarantine.to_string_lossy().into_owned()),
                        write_error: None,
                    },
                ))
            }
        }
    }

    fn quarantine_path(file: &Path) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut name = file.as_os_str().to_owned();
        name.push(format!(".corrupt-{stamp}"));
        PathBuf::from(name)
    }

    fn with(file: PathBuf, settings: Settings, health: Health) -> Self {
        Self {
            file,
            state: Mutex::new(State { settings, health }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        // A poisoned lock means a previous holder panicked mid-update. The
        // settings in there are a sanitised value either way, so recovering is
        // better than propagating a panic into every later call.
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn get(&self) -> Settings {
        self.lock().settings.clone()
    }

    pub fn health(&self) -> Health {
        self.lock().health.clone()
    }

    /// Mutate under the lock and persist.
    ///
    /// The draft is a private copy, so a mutator that leaves it inconsistent
    /// cannot be committed, and the live settings are replaced only once the
    /// write has reached disk.
    pub fn update<T>(&self, mutate: impl FnOnce(&mut Settings) -> T) -> Result<T> {
        let mut state = self.lock();
        let mut draft = state.settings.clone();
        let outcome = mutate(&mut draft);
        draft.schema = SCHEMA_VERSION;
        let next = sanitize(&serde_json::to_value(&draft).unwrap_or(Value::Null));

        match self.persist(&next) {
            Ok(()) => {
                state.settings = next;
                state.health.write_error = None;
                Ok(outcome)
            }
            Err(error) => {
                // Surface it. Swallowing this is how a read-only or full disk
                // becomes a silent, permanent loss of everything the user
                // changes afterwards.
                state.health.write_error = Some(WriteError {
                    code: error.code.as_str().to_owned(),
                    message: error.detail.clone(),
                });
                Err(error)
            }
        }
    }

    fn persist(&self, next: &Settings) -> Result<()> {
        // Keep the last known-good copy before replacing it. This is the file
        // `open` falls back to, and it is what makes a torn write survivable.
        if self.file.exists() {
            let _ = fs::copy(&self.file, Self::backup_path(&self.file));
        }
        write_json_atomic(&self.file, next)
    }
}
