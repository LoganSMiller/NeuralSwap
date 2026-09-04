// Tests may panic freely: a broken fixture should abort the run, loudly.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

//! End-to-end folder scanning, built out of the real PE fixtures in `spec/`.
//!
//! The unit tests next to the scanner cover the policy in isolation. These lay
//! the fixtures out as an actual game folder - the shapes engines really ship,
//! with a launcher and an uninstaller beside the game and assets buried
//! underneath - and check the scanner reaches the right conclusion about the
//! tree as a whole.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use neuralswap_core::jobs::Cancel;
use neuralswap_core::pe::PeCache;
use neuralswap_core::scan::{scan_folder, Api, EmptyReason, RuntimeKind};

fn spec_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../spec")
        .canonicalize()
        .expect("spec/ not found - run `npm run vectors`")
}

/// Copy a PE fixture into a game tree under a chosen name.
fn place(root: &Path, rel: &str, fixture: &str) {
    let target = root.join(rel);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::copy(spec_root().join(fixture), &target)
        .unwrap_or_else(|error| panic!("copy {fixture} -> {rel}: {error}"));
}

fn write(root: &Path, rel: &str, bytes: &[u8]) {
    let target = root.join(rel);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(&target, bytes).expect("write");
}

#[test]
fn an_unreal_shaped_folder_yields_the_game_and_not_the_launcher() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let root = scratch.path();

    // The real executable, buried where Unreal puts it.
    place(
        root,
        "Game/Binaries/Win64/Game-Win64-Shipping.exe",
        "pe/x64-d3d12.pe.bin",
    );
    // A launcher stub beside it, which must be excluded by name.
    place(root, "GameLauncher.exe", "pe/x86-d3d11.pe.bin");
    // An uninstaller, likewise.
    place(root, "unins000.exe", "pe/x86-d3d11.pe.bin");
    // An assets tree that must not be walked, holding something that would
    // otherwise look like a candidate.
    place(root, "Game/Content/Paks/decoy.exe", "pe/x64-d3d12.pe.bin");
    // Non-PE noise.
    write(root, "Game/Content/Paks/pakchunk0.pak", &[0u8; 4096]);

    let cache = Mutex::new(PeCache::new_empty());
    let scan = scan_folder(root, &cache, &Cancel::new());

    assert_eq!(scan.reason, None, "expected a usable candidate");
    let chosen = scan.chosen.expect("a chosen candidate");
    let candidate = scan.candidates.get(chosen).expect("candidate");

    assert_eq!(
        candidate.rel.replace('\\', "/"),
        "Game/Binaries/Win64/Game-Win64-Shipping.exe"
    );
    assert_eq!(candidate.bitness, 64);
    assert_eq!(candidate.api.as_ref().map(|v| v.api), Some(Api::Dxgi));
    assert_eq!(
        candidate.api.as_ref().map(|v| v.label.as_str()),
        Some("DirectX 12")
    );

    // The uninstaller is an unambiguous helper, so it is excluded outright and
    // recorded, not silently lost.
    let excluded: Vec<String> = scan.excluded.iter().map(|e| e.replace('\\', "/")).collect();
    assert!(excluded.iter().any(|e| e == "unins000.exe"), "{excluded:?}");

    // `GameLauncher.exe` is only *probably* a launcher - the prefix list
    // cannot see a helper word at the end of a name - so it is offered as a
    // demoted candidate rather than hidden. A wrong exclusion would mean a
    // game that is never detected at all.
    let launcher = scan
        .candidates
        .iter()
        .find(|c| c.rel.contains("GameLauncher"))
        .expect("the launcher should still be offered");
    assert!(
        launcher.likely_helper,
        "it should be flagged as a probable helper"
    );
    // And it must not be the recommendation.
    assert_ne!(
        scan.candidates.get(chosen).map(|c| &c.rel),
        Some(&launcher.rel)
    );

    // The decoy under Content/Paks was never reached.
    assert!(
        !scan.candidates.iter().any(|c| c.rel.contains("decoy")),
        "the assets tree was walked: {:?}",
        scan.candidates
    );
}

#[test]
fn a_folder_of_only_helpers_says_so() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let root = scratch.path();
    place(root, "setup.exe", "pe/x64-d3d12.pe.bin");
    place(root, "vc_redist.x64.exe", "pe/x86-d3d11.pe.bin");

    let cache = Mutex::new(PeCache::new_empty());
    let scan = scan_folder(root, &cache, &Cancel::new());

    assert!(scan.candidates.is_empty());
    // "Only helpers" is actionable; "no executable" would be a lie here.
    assert_eq!(scan.reason, Some(EmptyReason::OnlyHelpers));
    assert_eq!(scan.excluded.len(), 2);
}

#[test]
fn an_executable_with_no_graphics_api_is_reported_as_such() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let root = scratch.path();
    // A valid PE that imports nothing graphical.
    place(root, "Tool.exe", "pe/no-imports.pe.bin");

    let cache = Mutex::new(PeCache::new_empty());
    let scan = scan_folder(root, &cache, &Cancel::new());

    assert!(scan.candidates.is_empty());
    assert_eq!(scan.reason, Some(EmptyReason::NoGraphicsExecutable));
}

#[test]
fn a_folder_with_no_executables_at_all_is_distinguished() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let root = scratch.path();
    write(root, "readme.txt", b"nothing here");
    write(root, "data/assets.bin", &[0u8; 1024]);

    let cache = Mutex::new(PeCache::new_empty());
    let scan = scan_folder(root, &cache, &Cancel::new());
    assert_eq!(scan.reason, Some(EmptyReason::NoExecutable));
}

#[test]
fn a_path_that_is_not_a_folder_is_reported_rather_than_panicking() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let missing = scratch.path().join("does-not-exist");
    let cache = Mutex::new(PeCache::new_empty());
    let scan = scan_folder(&missing, &cache, &Cancel::new());
    assert_eq!(scan.reason, Some(EmptyReason::Unreadable));
}

#[test]
fn existing_nvidia_runtime_files_are_found() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let root = scratch.path();
    place(root, "Game.exe", "pe/x64-d3d12.pe.bin");
    // The runtime files a game with native DLSS already ships.
    place(root, "nvngx_dlss.dll", "pe/x64-d3d12.pe.bin");
    place(
        root,
        "Engine/Binaries/sl.interposer.dll",
        "pe/x64-d3d12.pe.bin",
    );

    let cache = Mutex::new(PeCache::new_empty());
    let scan = scan_folder(root, &cache, &Cancel::new());

    let kinds: Vec<RuntimeKind> = scan.runtime_files.iter().map(|f| f.kind).collect();
    assert!(
        kinds.contains(&RuntimeKind::Dlss),
        "{:?}",
        scan.runtime_files
    );
    assert!(
        kinds.contains(&RuntimeKind::Streamline),
        "{:?}",
        scan.runtime_files
    );
    // A runtime DLL is not a candidate executable.
    assert!(!scan.candidates.iter().any(|c| c.rel.contains("nvngx")));
}

#[test]
fn several_candidates_are_offered_best_first() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let root = scratch.path();
    // A DX12 build and a DX11 build, as titles shipping both modes do.
    place(root, "Game_DX12.exe", "pe/x64-d3d12.pe.bin");
    place(root, "Game_DX11.exe", "pe/x86-d3d11.pe.bin");

    let cache = Mutex::new(PeCache::new_empty());
    let scan = scan_folder(root, &cache, &Cancel::new());

    assert_eq!(scan.candidates.len(), 2, "{:?}", scan.candidates);
    // Both are offered - the person may want the other one - but DX12 leads.
    let first = scan.candidates.first().expect("first");
    assert_eq!(
        first.api.as_ref().map(|v| v.label.as_str()),
        Some("DirectX 12")
    );
    assert_eq!(scan.chosen, Some(0));
}

#[test]
fn a_rescan_of_an_unchanged_folder_is_answered_from_cache() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let root = scratch.path();
    place(root, "Game.exe", "pe/x64-d3d12.pe.bin");
    place(root, "Other.exe", "pe/x86-d3d11.pe.bin");

    let cache = Mutex::new(PeCache::new_empty());
    let first = scan_folder(root, &cache, &Cancel::new());
    assert!(
        first.stats.binaries_parsed >= 2,
        "expected the first pass to read both, parsed {}",
        first.stats.binaries_parsed
    );

    let second = scan_folder(root, &cache, &Cancel::new());
    assert_eq!(first.candidates, second.candidates);
    // Nothing changed, so the rescan parsed nothing at all: the cache served
    // it. An earlier version consulted the cache only *after* the parallel
    // pass, which left it write-only and made every rescan a full re-read.
    assert_eq!(
        second.stats.binaries_parsed, 0,
        "the rescan re-parsed {} binaries",
        second.stats.binaries_parsed
    );
    assert!(second.stats.cache_hits >= 2);
}

#[test]
fn a_cancelled_scan_stops_instead_of_finishing() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let root = scratch.path();
    for index in 0..40 {
        place(root, &format!("Game{index}.exe"), "pe/x64-d3d12.pe.bin");
    }

    let cancel = Cancel::new();
    cancel.cancel();
    let cache = Mutex::new(PeCache::new_empty());
    let scan = scan_folder(root, &cache, &cancel);

    // Already cancelled before it started: it must come back promptly with
    // nothing rather than walking the whole tree.
    assert!(
        scan.candidates.len() < 40,
        "a cancelled scan produced {} candidates",
        scan.candidates.len()
    );
}
