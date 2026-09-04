//! Walking one game folder and deciding what it is.
//!
//! The output is deliberately a *list* of candidates with a recommended one,
//! not a single answer. Plenty of folders hold several plausible executables -
//! a 32-bit and a 64-bit build, a DX11 and a DX12 mode, a launcher next to the
//! game - and the person in front of the window is better placed to pick than
//! any heuristic. The heuristic's job is to be right by default and to show
//! its reasoning when it is not.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::jobs::{pooled_map, Cancel};
use crate::pe::{PeCache, Request};

use super::api::{self, Api, Verdict, MARKERS};
use super::candidates::{
    is_likely_helper, is_probably_not_a_game, should_skip_content, should_skip_dir,
};

/// Detection generation. Bumping it invalidates every cached verdict, which is
/// what makes a fix to the rules above take effect on folders already scanned.
pub const RULES: i64 = 1;

/// How deep to walk. Engines bury the real executable a few levels down
/// (`Game/Binaries/Win64`); nothing legitimate is deeper than this.
const MAX_DEPTH: usize = 8;

/// A ceiling on files examined, so a folder someone pointed at their whole
/// drive cannot turn one scan into an afternoon.
const MAX_FILES: usize = 40_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    /// Path relative to the scanned folder, as a manifest would record it.
    pub rel: String,
    pub bitness: u8,
    pub api: Option<Verdict>,
    pub size: u64,
    pub file_version: Option<String>,
    /// The name looks like a launcher or helper, but not certainly enough to
    /// exclude it. Ranked last, and worth labelling in the UI.
    pub likely_helper: bool,
}

/// Why a folder produced nothing, in terms a person can act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EmptyReason {
    /// No `.exe` at all - probably not a game folder.
    NoExecutable,
    /// Executables, but none of them talk to a graphics API.
    NoGraphicsExecutable,
    /// Only installers, launchers and helpers were found.
    OnlyHelpers,
    /// The walk hit its file ceiling before finding anything.
    TooManyFiles,
    /// The folder could not be read.
    Unreadable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderScan {
    pub dir: PathBuf,
    /// Every executable that talks to a graphics API, best first.
    pub candidates: Vec<Candidate>,
    /// Index into `candidates` of the recommended target.
    pub chosen: Option<usize>,
    pub reason: Option<EmptyReason>,
    /// NVIDIA runtime files already present, relative to the folder.
    pub runtime_files: Vec<RuntimeFile>,
    /// Executables that were found but excluded, for diagnostics. A user
    /// asking "why didn't it find my game" needs to see what was skipped.
    pub excluded: Vec<String>,
    /// Directory entries examined, and how long each phase took. A scan that
    /// felt slow is not diagnosable without knowing whether the cost was the
    /// walk or the header reads.
    pub stats: ScanStats,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanStats {
    pub entries_examined: usize,
    pub directories_walked: usize,
    pub binaries_parsed: usize,
    pub cache_hits: u64,
    pub walk_ms: u64,
    pub parse_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeFile {
    pub rel: String,
    pub kind: RuntimeKind,
    pub version: Option<String>,
    pub provenance: Provenance,
}

/// Evidence about where a runtime file came from.
///
/// This matters because the presence of an `nvngx` DLL is **not** proof that a
/// game has native NGX calls - somebody may simply have copied one in.
/// Offering the native route on that basis produces an install that cannot
/// work, and a user with no idea why.
///
/// These name the *evidence*, not a conclusion. Nothing here can know what a
/// developer shipped; the authoritative answers are our own install manifest
/// (for files we placed) and an Authenticode check (for whether a file is a
/// genuine NVIDIA build), and neither exists yet.
///
/// Modification time was tried first and does not work: Windows `CopyFile`
/// preserves the source timestamp, so a DLL taken from an NVIDIA package keeps
/// its original build date and looks *older* than the game rather than newer.
/// Tested against a real install with a known hand-placed file, every runtime
/// came back indistinguishable. Version cohorts do work, because a game ships
/// its runtimes as a matched set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Provenance {
    /// Same version as the other runtimes beside it, and beside the game's
    /// executable: consistent with the set the game installed.
    ConsistentWithSiblings,
    /// A different version from the other runtimes of its kind in the same
    /// folder. A game ships a matched set, so the odd one out was replaced.
    VersionDiffersFromSiblings,
    /// Not in the same folder as any executable we found. The loader looks
    /// beside the executable, so a copy elsewhere was probably placed by hand.
    NotBesideExecutable,
    /// Recorded in our own install manifest.
    OurInstall,
    /// Nothing to compare it against.
    Unknown,
}

/// The version shared by most files of one kind in one folder, if there is one.
fn modal_version(versions: &[Option<String>]) -> Option<String> {
    let mut tally: Vec<(String, usize)> = Vec::new();
    for version in versions.iter().flatten() {
        match tally.iter_mut().find(|(seen, _)| seen == version) {
            Some((_, count)) => *count += 1,
            None => tally.push((version.clone(), 1)),
        }
    }
    // A single sample is not a cohort: with one file there is no "odd one out".
    let total: usize = tally.iter().map(|(_, count)| count).sum();
    if total < 2 {
        return None;
    }
    tally
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(version, _)| version)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RuntimeKind {
    /// `nvngx_dlss.dll` and friends - the upscaler itself.
    Dlss,
    /// `sl.*.dll` - Streamline, which brokers the features.
    Streamline,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The folder part of a relative path, lower-cased with one separator style,
/// so `bin\x64\a.dll` and `bin/x64/b.dll` compare as the same folder.
fn parent_key(rel: &str) -> String {
    let normalised = rel.replace('\\', "/").to_lowercase();
    match normalised.rfind('/') {
        Some(at) => normalised.get(..at).unwrap_or_default().to_owned(),
        None => String::new(),
    }
}

fn classify_runtime(file_name: &str) -> Option<RuntimeKind> {
    let lower = file_name.to_lowercase();
    if !lower.ends_with(".dll") {
        return None;
    }
    if lower == "nvngx.dll" || lower == "_nvngx.dll" || lower.starts_with("nvngx_dlss") {
        return Some(RuntimeKind::Dlss);
    }
    if lower.starts_with("sl.") {
        return Some(RuntimeKind::Streamline);
    }
    None
}

struct Found {
    path: PathBuf,
    rel: String,
    size: u64,
}

/// Walk the folder, collecting executables worth opening and runtime files
/// already present.
struct Walk {
    executables: Vec<Found>,
    runtimes: Vec<Found>,
    excluded: Vec<String>,
    hit_ceiling: bool,
    examined: usize,
    directories: usize,
}

fn collect(dir: &Path, cancel: &Cancel) -> Walk {
    let mut executables: Vec<Found> = Vec::new();
    let mut runtimes: Vec<Found> = Vec::new();
    let mut excluded: Vec<String> = Vec::new();
    let mut examined = 0usize;
    let mut directories = 0usize;
    let mut hit_ceiling = false;

    // Xbox/Game Pass installs put the whole game under `Content`, which is
    // otherwise an assets-only tree worth skipping.
    let xbox_layout =
        dir.join("MicrosoftGame.config").exists() || dir.join("appxmanifest.xml").exists();

    let mut queue: VecDeque<(PathBuf, usize)> = VecDeque::new();
    queue.push_back((dir.to_path_buf(), 0));

    while let Some((current, depth)) = queue.pop_front() {
        if cancel.is_cancelled() {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            if examined >= MAX_FILES {
                hit_ceiling = true;
                break;
            }
            examined += 1;

            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(kind) = entry.file_type() else {
                continue;
            };

            if kind.is_dir() {
                if depth + 1 > MAX_DEPTH
                    || should_skip_dir(&name)
                    || should_skip_content(&name, xbox_layout)
                {
                    continue;
                }
                queue.push_back((entry.path(), depth + 1));
                directories += 1;
                continue;
            }
            if !kind.is_file() {
                continue;
            }

            // Classify by name first. A game folder is overwhelmingly asset
            // files, and building a relative path, joining the full path and
            // reading metadata for every one of tens of thousands of them
            // costs far more than the handful we actually care about.
            let runtime = classify_runtime(&name);
            let is_exe = name.len() > 4 && name[name.len() - 4..].eq_ignore_ascii_case(".exe");
            if runtime.is_none() && !is_exe {
                continue;
            }

            let path = entry.path();
            let rel = path
                .strip_prefix(dir)
                .map(|r| r.to_string_lossy().into_owned())
                .unwrap_or_else(|_| name.clone());

            if runtime.is_some() {
                runtimes.push(Found { path, rel, size: 0 });
                continue;
            }
            if is_probably_not_a_game(&name) {
                excluded.push(rel);
                continue;
            }
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            executables.push(Found { path, rel, size });
        }
        if hit_ceiling {
            break;
        }
    }
    Walk {
        executables,
        runtimes,
        excluded,
        hit_ceiling,
        examined,
        directories,
    }
}

/// How good a candidate is, higher being better.
///
/// The ordering is: real import-table evidence over a string; a newer Direct3D
/// over an older one; then the larger file, because across a very wide range
/// of engines the game is the big binary and the stubs beside it are small.
/// Size is the tie-breaker rather than the signal - it is the weakest of the
/// three and only decides between candidates that are otherwise equal.
fn rank(candidate: &Candidate) -> (u8, u8, u8, u64) {
    // A suspected launcher sorts below every ordinary candidate, whatever its
    // API or size.
    let trusted = u8::from(!candidate.likely_helper);
    let evidence = match &candidate.api {
        Some(verdict) if !verdict.from_marker => 2,
        Some(_) => 1,
        None => 0,
    };
    let api_score = match candidate.api.as_ref().map(|v| (v.api, v.label.as_str())) {
        Some((Api::Dxgi, "DirectX 12")) => 6,
        Some((Api::Dxgi, "DirectX 11")) => 5,
        Some((Api::Dxgi, _)) => 4,
        Some((Api::Vulkan, _)) => 4,
        Some((Api::D3d9, _)) => 3,
        Some((Api::OpenGl, _)) => 3,
        Some((Api::D3d8, _)) => 2,
        // DirectX 10 has no supported route, so it must never outrank a
        // candidate that does.
        Some((Api::D3d10, _)) => 1,
        None => 0,
    };
    (trusted, evidence, api_score, candidate.size)
}

/// Scan one folder. `cache` makes a rescan of unchanged files nearly free.
/// Scan one folder.
///
/// The cache is behind a `Mutex` rather than taken as `&mut` because the
/// parallel pass consults it. An earlier version read `&mut PeCache` and so
/// could not share it with the worker threads: it parsed everything, then
/// wrote the results into the cache afterwards. The cache was therefore
/// write-only, and a rescan cost exactly as much as the first scan - which is
/// the opposite of the whole point of having one. Locking around the lookup
/// and the insert, while parsing outside the lock, keeps the critical sections
/// tiny and makes a rescan cost a `stat` per file.
pub fn scan_folder(dir: &Path, cache: &Mutex<PeCache>, cancel: &Cancel) -> FolderScan {
    if !dir.is_dir() {
        return FolderScan {
            dir: dir.to_path_buf(),
            candidates: Vec::new(),
            chosen: None,
            reason: Some(EmptyReason::Unreadable),
            runtime_files: Vec::new(),
            excluded: Vec::new(),
            stats: ScanStats::default(),
        };
    }

    let walk_started = Instant::now();
    let walk = collect(dir, cancel);
    let walk_ms = walk_started.elapsed().as_millis() as u64;

    let Walk {
        executables,
        runtimes,
        excluded,
        hit_ceiling,
        examined,
        directories,
    } = walk;

    let request = Request {
        markers: MARKERS,
        rules: RULES,
        ..Request::default()
    };

    // Reading headers is IO-bound on a lot of small reads, so candidates are
    // examined in parallel.
    let parse_started = Instant::now();
    let parsed = AtomicUsize::new(0);
    let summaries = pooled_map(&executables, 4, cancel, |found, _| {
        // Ask the cache first. Only the lookup and the insert hold the lock;
        // the expensive part happens between them, unlocked.
        if let Some(hit) = lock(cache).peek(&found.path, &request) {
            return Some((found.rel.clone(), hit));
        }
        parsed.fetch_add(1, Ordering::Relaxed);
        let summary = crate::pe::summarize(&found.path, &request);
        lock(cache).remember(&found.path, &request, summary.clone());
        summary.map(|value| (found.rel.clone(), value))
    });
    let parse_ms = parse_started.elapsed().as_millis() as u64;

    let mut candidates: Vec<Candidate> = Vec::new();
    for (index, entry) in summaries.into_iter().enumerate() {
        let Some((rel, summary)) = entry else {
            continue;
        };
        let size = executables.get(index).map(|found| found.size).unwrap_or(0);
        let verdict = api::detect(&summary.imports, &summary.markers);
        if verdict.is_none() {
            continue;
        }
        let likely_helper = Path::new(&rel)
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(is_likely_helper);
        candidates.push(Candidate {
            rel,
            bitness: summary.bitness,
            api: verdict,
            size,
            file_version: summary.file_version,
            likely_helper,
        });
    }

    candidates.sort_by(|a, b| rank(b).cmp(&rank(a)).then_with(|| a.rel.cmp(&b.rel)));
    let chosen = (!candidates.is_empty()).then_some(0);

    let reason = if !candidates.is_empty() {
        None
    } else if hit_ceiling {
        Some(EmptyReason::TooManyFiles)
    } else if !executables.is_empty() {
        Some(EmptyReason::NoGraphicsExecutable)
    } else if !excluded.is_empty() {
        Some(EmptyReason::OnlyHelpers)
    } else {
        Some(EmptyReason::NoExecutable)
    };

    // Folders that hold an executable we found. The loader looks beside the
    // executable, so runtimes anywhere else were probably placed by hand.
    let exe_dirs: Vec<String> = candidates
        .iter()
        .map(|candidate| parent_key(&candidate.rel))
        .collect();

    // Read each runtime's version once, then judge it against the other
    // runtimes of the same kind in the same folder.
    let mut runtime_files: Vec<RuntimeFile> = runtimes
        .iter()
        .filter_map(|found| {
            let name = Path::new(&found.rel).file_name()?.to_str()?;
            let kind = classify_runtime(name)?;
            Some(RuntimeFile {
                rel: found.rel.clone(),
                kind,
                version: crate::pe::PeFile::with(&found.path, |pe| pe.file_version(), None),
                provenance: Provenance::Unknown,
            })
        })
        .collect();

    for index in 0..runtime_files.len() {
        let Some(file) = runtime_files.get(index) else {
            continue;
        };
        let folder = parent_key(&file.rel);
        let kind = file.kind;
        let version = file.version.clone();

        let cohort: Vec<Option<String>> = runtime_files
            .iter()
            .filter(|other| other.kind == kind && parent_key(&other.rel) == folder)
            .map(|other| other.version.clone())
            .collect();

        let verdict = match modal_version(&cohort) {
            Some(expected) if version.as_deref() != Some(expected.as_str()) => {
                Provenance::VersionDiffersFromSiblings
            }
            _ if !exe_dirs.contains(&folder) => Provenance::NotBesideExecutable,
            Some(_) => Provenance::ConsistentWithSiblings,
            None => Provenance::Unknown,
        };
        if let Some(file) = runtime_files.get_mut(index) {
            file.provenance = verdict;
        }
    }

    FolderScan {
        dir: dir.to_path_buf(),
        candidates,
        chosen,
        reason,
        runtime_files,
        excluded,
        stats: ScanStats {
            entries_examined: examined,
            directories_walked: directories,
            binaries_parsed: parsed.load(Ordering::Relaxed),
            cache_hits: lock(cache).stats().hits,
            walk_ms,
            parse_ms,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn versions(list: &[&str]) -> Vec<Option<String>> {
        list.iter()
            .map(|v| (!v.is_empty()).then(|| (*v).to_owned()))
            .collect()
    }

    #[test]
    fn a_cohort_needs_more_than_one_sample() {
        // With one file there is no "odd one out" to find.
        assert_eq!(modal_version(&versions(&["310.1.0.0"])), None);
        assert_eq!(modal_version(&versions(&[])), None);
        assert_eq!(modal_version(&versions(&[""])), None);
    }

    #[test]
    fn the_majority_version_is_the_cohort() {
        // The real shape found in a Cyberpunk install: three runtimes at one
        // version and a hand-placed fourth at another.
        let found = modal_version(&versions(&[
            "310.1.0.0",
            "310.1.0.0",
            "310.1.0.0",
            "310.8.0.0",
        ]));
        assert_eq!(found.as_deref(), Some("310.1.0.0"));
    }

    #[test]
    fn a_file_with_no_version_does_not_dominate_the_cohort() {
        // An unreadable version resource must not be counted as a version, or
        // a folder of unversioned files would flag the real ones as odd.
        let found = modal_version(&versions(&["", "", "2.7.1.0", "2.7.1.0"]));
        assert_eq!(found.as_deref(), Some("2.7.1.0"));
    }

    #[test]
    fn folder_comparison_ignores_separator_style_and_case() {
        assert_eq!(parent_key("bin\\x64\\nvngx_dlss.dll"), "bin/x64");
        assert_eq!(parent_key("bin/x64/sl.dlss.dll"), "bin/x64");
        assert_eq!(parent_key("BIN\\X64\\a.dll"), "bin/x64");
        // A file at the root has no folder, and that is a real value to
        // compare - a runtime in the game root is exactly the suspicious case.
        assert_eq!(parent_key("nvngx_dlss.dll"), "");
    }

    #[test]
    fn runtime_files_are_classified_by_name() {
        assert_eq!(classify_runtime("nvngx_dlss.dll"), Some(RuntimeKind::Dlss));
        assert_eq!(classify_runtime("nvngx_dlssg.dll"), Some(RuntimeKind::Dlss));
        assert_eq!(classify_runtime("nvngx.dll"), Some(RuntimeKind::Dlss));
        assert_eq!(classify_runtime("_nvngx.dll"), Some(RuntimeKind::Dlss));
        assert_eq!(
            classify_runtime("sl.interposer.dll"),
            Some(RuntimeKind::Streamline)
        );
        // Not runtimes.
        assert_eq!(classify_runtime("game.exe"), None);
        assert_eq!(classify_runtime("nvngx_dlss.txt"), None);
        assert_eq!(classify_runtime("slime.dll"), None);
    }

    fn candidate(label: &str, api: Api, from_marker: bool, size: u64) -> Candidate {
        Candidate {
            rel: format!("{label}.exe"),
            bitness: 64,
            api: Some(Verdict {
                api,
                label: label.to_owned(),
                from_marker,
            }),
            size,
            file_version: None,
            likely_helper: false,
        }
    }

    #[test]
    fn import_evidence_outranks_a_marker_even_for_a_newer_api() {
        // A DX11 verdict from the import table beats a DX12 verdict from a
        // string, because the string may be a leftover code path.
        let imported = candidate("DirectX 11", Api::Dxgi, false, 1000);
        let guessed = candidate("DirectX 12", Api::Dxgi, true, 100_000_000);
        assert!(rank(&imported) > rank(&guessed));
    }

    #[test]
    fn a_newer_direct3d_outranks_an_older_one() {
        let twelve = candidate("DirectX 12", Api::Dxgi, false, 1000);
        let nine = candidate("DirectX 9", Api::D3d9, false, 100_000_000);
        assert!(rank(&twelve) > rank(&nine));
    }

    #[test]
    fn directx_10_never_outranks_a_supportable_api() {
        // There is no injection route for DX10, so it must not win.
        let ten = candidate("DirectX 10", Api::D3d10, false, 100_000_000);
        let eight = candidate("DirectX 8", Api::D3d8, false, 1000);
        assert!(rank(&eight) > rank(&ten));
    }

    #[test]
    fn size_only_breaks_a_tie() {
        let big = candidate("DirectX 12", Api::Dxgi, false, 90_000_000);
        let small = candidate("DirectX 12", Api::Dxgi, false, 500_000);
        assert!(rank(&big) > rank(&small));
    }
}
