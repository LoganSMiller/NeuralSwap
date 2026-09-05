// Tests may panic freely: a broken fixture should abort the run, loudly.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

//! The whole install path, end to end, against real PE files.
//!
//! The unit tests use short stub files, which is right for exercising the
//! decisions but means the version reader never runs on anything real - and a
//! version is what decides whether an install is an upgrade or a downgrade.
//! Here the "runtimes" are genuine signed system DLLs copied under runtime
//! names, so `VS_FIXEDFILEINFO` parsing, hashing of megabyte-scale files, the
//! atomic replace, the manifest and the restore all run on the shape they will
//! actually meet.
//!
//! Windows-only, and skipped rather than failed where the source DLLs are not
//! present: this asserts against the machine it runs on.

use std::path::{Path, PathBuf};

use neuralswap_core::install::{
    apply, manifest, package, plan, restore, FileStatus, StepAction, StepReason,
};
use neuralswap_core::jobs::Cancel;

struct Bench {
    _root: tempfile::TempDir,
    game: PathBuf,
    source: PathBuf,
    journals: PathBuf,
    backups: PathBuf,
    manifests: PathBuf,
}

/// Two different real DLLs, so "replace" has something to replace and the two
/// sides carry different versions and different bytes.
fn real_dlls() -> Option<(PathBuf, PathBuf)> {
    let candidates = [
        "C:\\Windows\\System32\\kernel32.dll",
        "C:\\Windows\\System32\\user32.dll",
        "C:\\Windows\\System32\\shell32.dll",
        "C:\\Windows\\System32\\advapi32.dll",
    ];
    let found: Vec<PathBuf> = candidates
        .iter()
        .map(PathBuf::from)
        .filter(|path| path.is_file())
        .collect();
    match found.as_slice() {
        [first, second, ..] => Some((first.clone(), second.clone())),
        _ => None,
    }
}

fn bench(newer: &Path, older: &Path) -> Bench {
    let root = tempfile::tempdir().expect("tempdir");
    let game = root.path().join("game");
    let source = root.path().join("package");
    std::fs::create_dir_all(game.join("bin/x64")).expect("game dirs");
    std::fs::create_dir_all(&source).expect("source dir");

    // The package offers one runtime; the game already has a different one at
    // the same path.
    std::fs::copy(newer, source.join("nvngx_dlss.dll")).expect("stage package");
    std::fs::copy(older, game.join("bin/x64/nvngx_dlss.dll")).expect("stage game");

    Bench {
        game,
        source,
        journals: root.path().join("journal"),
        backups: root.path().join("backups"),
        manifests: root.path().join("installs"),
        _root: root,
    }
}

#[test]
fn a_real_runtime_swap_installs_verifies_and_restores() {
    let Some((newer, older)) = real_dlls() else {
        return;
    };
    let bench = bench(&newer, &older);
    let cancel = Cancel::new();

    // -- read both sides -------------------------------------------------
    let offered = package::read_package(&bench.source).expect("read package");
    assert_eq!(offered.len(), 1);
    assert!(
        offered[0].version.is_some(),
        "a real system DLL should carry a version resource"
    );
    assert_eq!(
        offered[0].size,
        std::fs::metadata(&newer).expect("meta").len()
    );

    let present = package::read_present(&bench.game, "bin/x64", &[]).expect("read present");
    assert_eq!(present.len(), 1);
    assert!(present[0].version.is_some());
    assert!(!present[0].managed, "nothing has been installed yet");

    let original_bytes = std::fs::read(bench.game.join("bin/x64/nvngx_dlss.dll")).expect("read");
    let original_hash = neuralswap_core::hash::hash_bytes(&original_bytes);

    // -- plan ------------------------------------------------------------
    let planned = plan::build_plan(&plan::PlanInput {
        route: plan::Route::NativeDll,
        install_dir: "bin/x64".to_owned(),
        present: present.clone(),
        pkg: offered.clone(),
    })
    .expect("plan");

    assert_eq!(planned.steps.len(), 1);
    let step = &planned.steps[0];
    assert_eq!(step.rel, "bin/x64/nvngx_dlss.dll");
    assert_eq!(step.action, StepAction::Replace);
    // Two different real DLLs: the versions differ, so the plan must have
    // reached a verdict about which direction, not fallen back to "unknown".
    assert!(
        matches!(
            step.reason,
            StepReason::Upgrade | StepReason::Downgrade | StepReason::SameVersionDifferentBytes
        ),
        "unexpected reason for two real DLLs: {:?}",
        step.reason
    );
    assert!(planned.backup_bytes > 0, "a replacement must be backed up");
    // It is replacing a file we did not install, and it should say so.
    assert!(planned
        .warnings
        .iter()
        .any(|warning| warning.code == plan::WarningCode::ReplacesUnmanagedFile));

    // -- install ---------------------------------------------------------
    let outcome = apply::apply(&apply::Request {
        game_dir: &bench.game,
        plan: &planned,
        source_dir: &bench.source,
        journal_root: &bench.journals,
        backup_root: &bench.backups,
        manifest_root: &bench.manifests,
        requires: None,
        layers: &neuralswap_core::install::layer::NoRegistry,
        cancel: &cancel,
    })
    .expect("apply");

    let applied = match outcome {
        apply::Outcome::Installed(applied) => applied,
        other => panic!("install did not succeed: {other:?}"),
    };
    assert_eq!(applied.installed, vec!["bin/x64/nvngx_dlss.dll"]);

    // The package's bytes are now in the game folder, byte for byte.
    assert_eq!(
        std::fs::read(bench.game.join("bin/x64/nvngx_dlss.dll")).expect("read back"),
        std::fs::read(&newer).expect("read package source")
    );
    // And the journal is gone, because the install committed.
    assert!(
        neuralswap_core::install::journal::survey(&bench.journals)
            .expect("survey")
            .is_empty(),
        "a committed install leaves no journal"
    );

    // -- verify ----------------------------------------------------------
    let record = manifest::load(&bench.manifests, &bench.game)
        .expect("load")
        .expect("a manifest");
    let integrity = manifest::verify(&record);
    assert!(integrity.intact, "{integrity:?}");
    assert_eq!(integrity.files[0].status, FileStatus::Intact);
    assert!(integrity.files[0].restorable);

    // The displaced original is kept, intact, outside the journal.
    let kept = record.files[0]
        .replaced
        .as_ref()
        .expect("the original is recorded");
    assert_eq!(kept.sha256, original_hash);
    assert_eq!(
        std::fs::read(&kept.backup).expect("read backup"),
        original_bytes
    );

    // A second plan against what is now on disk finds nothing to do, and knows
    // the file is ours.
    let after = package::read_present(&bench.game, "bin/x64", &record.managed_rels())
        .expect("read present");
    assert!(after[0].managed, "the manifest should claim this file");
    let replanned = plan::build_plan(&plan::PlanInput {
        route: plan::Route::NativeDll,
        install_dir: "bin/x64".to_owned(),
        present: after,
        pkg: offered,
    })
    .expect("plan");
    assert_eq!(replanned.changes, 0, "re-installing should be a no-op");

    // -- restore ---------------------------------------------------------
    let undone = restore::restore(&restore::Request {
        game_dir: &bench.game,
        manifest_root: &bench.manifests,
        cancel: &cancel,
    })
    .expect("restore");

    match undone {
        restore::Outcome::Restored(report) => {
            assert!(report.complete, "{report:?}");
            assert_eq!(report.files[0].action, restore::Action::RestoredOriginal);
        }
        other => panic!("expected a restore, got {other:?}"),
    }

    // The game's own file is back, byte for byte, and the record is cleared.
    assert_eq!(
        std::fs::read(bench.game.join("bin/x64/nvngx_dlss.dll")).expect("read back"),
        original_bytes
    );
    assert!(manifest::load(&bench.manifests, &bench.game)
        .expect("load")
        .is_none());
}

/// A game update overwriting our file is the case that makes verification
/// worth having, and the case a restore must decline to undo.
#[test]
fn a_patch_over_our_install_is_detected_and_not_reverted() {
    let Some((newer, older)) = real_dlls() else {
        return;
    };
    let bench = bench(&newer, &older);
    let cancel = Cancel::new();

    let planned = plan::build_plan(&plan::PlanInput {
        route: plan::Route::NativeDll,
        install_dir: "bin/x64".to_owned(),
        present: package::read_present(&bench.game, "bin/x64", &[]).expect("present"),
        pkg: package::read_package(&bench.source).expect("package"),
    })
    .expect("plan");

    let outcome = apply::apply(&apply::Request {
        game_dir: &bench.game,
        plan: &planned,
        source_dir: &bench.source,
        journal_root: &bench.journals,
        backup_root: &bench.backups,
        manifest_root: &bench.manifests,
        requires: None,
        layers: &neuralswap_core::install::layer::NoRegistry,
        cancel: &cancel,
    })
    .expect("apply");
    assert!(matches!(outcome, apply::Outcome::Installed(_)));

    // The patch: a third file lands where ours was.
    std::fs::write(
        bench.game.join("bin/x64/nvngx_dlss.dll"),
        b"what the game update installed",
    )
    .expect("simulate a patch");

    let record = manifest::load(&bench.manifests, &bench.game)
        .expect("load")
        .expect("a manifest");
    let integrity = manifest::verify(&record);
    assert!(!integrity.intact);
    assert_eq!(integrity.files[0].status, FileStatus::Changed);
    assert!(integrity.files[0].found_sha256.is_some());

    // Restoring must leave the patched file alone: writing a months-old backup
    // over it would be the undo causing the damage.
    let undone = restore::restore(&restore::Request {
        game_dir: &bench.game,
        manifest_root: &bench.manifests,
        cancel: &cancel,
    })
    .expect("restore");
    match undone {
        restore::Outcome::Restored(report) => {
            assert_eq!(report.files[0].action, restore::Action::LeftAlone);
        }
        other => panic!("expected a restore report, got {other:?}"),
    }
    assert_eq!(
        std::fs::read(bench.game.join("bin/x64/nvngx_dlss.dll")).expect("read"),
        b"what the game update installed"
    );
}
