use std::path::{Path, PathBuf};
use std::sync::Mutex;

use neuralswap_core::fsx::atomic::{read_to_string_or_none, write_json_atomic};
use neuralswap_core::jobs::{Cancel, Sweeper};
use neuralswap_core::pe::PeCache;
use neuralswap_core::scan::{scan_folder, FolderScan};

/// Owns the PE cache, the sweep token and the per-game job locks.
///
/// The cache is the reason a rescan is nearly free, and it is worth keeping
/// across launches: the app rescans on every start, and almost nothing has
/// changed since the last one. Persisting it turns the usual startup scan from
/// "read every header again" into "stat every file".
pub struct Scanner {
    cache: Mutex<PeCache>,
    cache_file: PathBuf,
    sweeper: Sweeper,
    /// Cancellation for the in-flight single-folder scan, so the UI can drop a
    /// scan it no longer cares about.
    cancel: Mutex<Cancel>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Scanner {
    /// Load the persisted cache, or start empty. A cache that cannot be read
    /// is not worth reporting: the only cost is a slower first scan.
    pub fn load(cache_file: PathBuf) -> Self {
        let cache = read_to_string_or_none(&cache_file)
            .ok()
            .flatten()
            .and_then(|text| serde_json::from_str::<PeCache>(&text).ok())
            .unwrap_or_default();

        Self {
            cache: Mutex::new(cache),
            cache_file,
            sweeper: Sweeper::new(),
            cancel: Mutex::new(Cancel::new()),
        }
    }

    /// Scan one folder. Blocking: callers hand this to a blocking thread.
    ///
    /// The cache is handed over as the `Mutex` itself, not a held guard: the
    /// scan consults it from several threads and takes the lock only around
    /// each lookup and insert.
    pub fn scan(&self, dir: &Path) -> FolderScan {
        let cancel = lock(&self.cancel).clone();
        let scan = scan_folder(dir, &self.cache, &cancel);
        self.persist_cache();
        scan
    }

    /// Cancel the in-flight scan and arm a fresh token for the next one.
    pub fn cancel(&self) {
        let mut current = lock(&self.cancel);
        current.cancel();
        *current = Cancel::new();
        self.sweeper.cancel();
    }

    pub fn cache_entries(&self) -> usize {
        lock(&self.cache).len()
    }

    /// Drop entries for files that no longer exist, so the cache cannot grow
    /// without bound as games are uninstalled.
    pub fn prune_cache(&self) -> usize {
        let removed = lock(&self.cache).prune();
        if removed > 0 {
            self.persist_cache();
        }
        removed
    }

    fn persist_cache(&self) {
        let snapshot = lock(&self.cache);
        // A cache that fails to save costs a slower next scan and nothing
        // else, so this is logged rather than surfaced to the user.
        if let Err(error) = write_json_atomic(&self.cache_file, &*snapshot) {
            log::warn!("could not save the scan cache: {error}");
        }
    }
}
