//! Everything the scanner needs to know about one binary, gathered in a single
//! open, plus a cache so that asking again is free.
//!
//! Rescanning a library is the common case - it happens on every launch, after
//! every install, and whenever a folder is added - and almost nothing has
//! changed between one scan and the next. Re-reading every header and
//! re-scanning every section each time is the bulk of that work, and all of it
//! is avoidable: a file whose size and modification time are unchanged cannot
//! have different imports than it did a minute ago.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::reader::PeFile;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeSummary {
    pub bitness: u8,
    pub machine: u16,
    /// Imported and delay-imported DLL names, lower-cased.
    pub imports: Vec<String>,
    pub file_version: Option<String>,
    /// Which of the requested markers were present, sorted.
    pub markers: Vec<String>,
    /// Which of the requested version-resource strings were present.
    pub version_strings: Vec<String>,
    /// Which of the requested named byte probes matched.
    pub probes: Vec<String>,
}

/// What to ask of a binary.
#[derive(Debug, Clone, Default)]
pub struct Request<'a> {
    /// ASCII entry-point / DLL-name strings to look for in mapped sections.
    pub markers: &'a [&'a str],
    /// Vendor strings to look for in the version resource.
    pub version_strings: &'a [&'a str],
    /// Named byte probes, e.g. the ReShade add-on loader signature.
    pub probes: &'a [(&'a str, &'a str)],
    /// Bumped whenever the requested sets change, so entries cached under an
    /// older question are not answered with a stale result.
    pub rules: i64,
}

pub fn summarize(path: &Path, request: &Request<'_>) -> Option<PeSummary> {
    PeFile::with(
        path,
        |pe| {
            let markers = pe.find_markers(request.markers);
            let version_strings = request
                .version_strings
                .iter()
                .filter(|text| pe.version_mentions(text))
                .map(|text| (*text).to_owned())
                .collect();
            let probes = request
                .probes
                .iter()
                .filter(|(_, needle)| pe.contains_bytes(needle.as_bytes()))
                .map(|(name, _)| (*name).to_owned())
                .collect();

            Some(PeSummary {
                bitness: pe.bitness(),
                machine: pe.machine(),
                imports: pe.import_names(),
                file_version: pe.file_version(),
                markers,
                version_strings,
                probes,
            })
        },
        None,
    )
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    size: u64,
    /// Milliseconds since the epoch. Stored as a number so the cache file is
    /// comparable across platforms and readable by a person.
    mtime_ms: i64,
    rules: i64,
    /// `None` records "this is not a parseable PE", which is worth
    /// remembering: it stops a folder of data files being re-examined on
    /// every single scan.
    summary: Option<PeSummary>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Stats {
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
}

/// Identity is (path, size, modification time). A game update changes at least
/// one of the latter two, so a patched executable is always re-read and an
/// untouched one never is.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PeCache {
    entries: BTreeMap<String, Entry>,
    #[serde(skip)]
    stats: Stats,
}

fn key_for(path: &Path) -> String {
    path.to_string_lossy().to_lowercase()
}

fn mtime_ms(metadata: &std::fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| match time.duration_since(UNIX_EPOCH) {
            Ok(delta) => i64::try_from(delta.as_millis()).ok(),
            // A file dated before 1970 is odd but not a reason to fail.
            Err(error) => i64::try_from(error.duration().as_millis()).ok().map(|v| -v),
        })
        .unwrap_or(0)
}

impl PeCache {
    /// An empty cache. `Default` is derived for the deserialised case.
    pub fn new_empty() -> Self {
        Self::default()
    }

    pub fn stats(&self) -> Stats {
        self.stats
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// A cached answer, if there is a fresh one, without parsing on a miss.
    ///
    /// Split from `summarize` so a parallel scan can consult the cache under a
    /// short lock, parse outside it, and store the result afterwards - rather
    /// than holding the lock for the whole parse or, worse, bypassing the
    /// cache entirely and leaving it write-only.
    pub fn peek(&mut self, path: &Path, request: &Request<'_>) -> Option<PeSummary> {
        let key = key_for(path);
        let metadata = std::fs::metadata(path).ok()?;
        if !metadata.is_file() {
            return None;
        }
        let cached = self.entries.get(&key)?;
        if cached.size != metadata.len()
            || cached.mtime_ms != mtime_ms(&metadata)
            || cached.rules != request.rules
        {
            return None;
        }
        self.stats.hits += 1;
        cached.summary.clone()
    }

    /// Store a result computed outside the lock.
    pub fn remember(&mut self, path: &Path, request: &Request<'_>, summary: Option<PeSummary>) {
        let Ok(metadata) = std::fs::metadata(path) else {
            return;
        };
        if !metadata.is_file() {
            return;
        }
        let key = key_for(path);
        if self.entries.contains_key(&key) {
            self.stats.evictions += 1;
        }
        self.entries.insert(
            key,
            Entry {
                size: metadata.len(),
                mtime_ms: mtime_ms(&metadata),
                rules: request.rules,
                summary,
            },
        );
        self.stats.misses += 1;
    }

    pub fn summarize(&mut self, path: &Path, request: &Request<'_>) -> Option<PeSummary> {
        let key = key_for(path);
        let Ok(metadata) = std::fs::metadata(path) else {
            // Gone since the directory walk. Forget any entry for it.
            self.entries.remove(&key);
            return None;
        };
        if !metadata.is_file() {
            return None;
        }
        let size = metadata.len();
        let mtime = mtime_ms(&metadata);

        if let Some(cached) = self.entries.get(&key) {
            if cached.size == size && cached.mtime_ms == mtime && cached.rules == request.rules {
                self.stats.hits += 1;
                return cached.summary.clone();
            }
            self.stats.evictions += 1;
        }

        let summary = summarize(path, request);
        self.entries.insert(
            key,
            Entry {
                size,
                mtime_ms: mtime,
                rules: request.rules,
                summary: summary.clone(),
            },
        );
        self.stats.misses += 1;
        summary
    }

    /// Drop entries for files that no longer exist, so the cache cannot grow
    /// without bound across a machine's lifetime.
    pub fn prune(&mut self) -> usize {
        let gone: Vec<String> = self
            .entries
            .keys()
            .filter(|key| !Path::new(key).exists())
            .cloned()
            .collect();
        for key in &gone {
            self.entries.remove(key);
        }
        gone.len()
    }

    /// Freshness of the whole cache, for a diagnostics report.
    pub fn newest_entry(&self) -> Option<SystemTime> {
        self.entries
            .values()
            .map(|entry| entry.mtime_ms)
            .max()
            .and_then(|ms| u64::try_from(ms).ok())
            .map(|ms| UNIX_EPOCH + std::time::Duration::from_millis(ms))
    }
}
