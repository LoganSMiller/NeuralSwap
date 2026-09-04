//! Putting a game back the way it was.
//!
//! This is what the backup store exists for, and it is the difference between
//! a tool somebody tries on something expendable and one they use on a game
//! they care about. An install that cannot be undone is a gamble.
//!
//! **No journal here, deliberately.** An install needs one because it has an
//! intermediate state that is meaningless on its own - a folder with two of
//! four DLLs replaced. A restore has no such state: every operation is an
//! atomic replace or a delete, each one independently correct, and the
//! manifest is removed only after all of them have finished. So an interrupted
//! restore is repaired by running it again, which is exactly what happens at
//! the next launch. Adding a journal would add a failure mode without removing
//! one.
//!
//! Two judgements here that a naive restore gets wrong, both of which would
//! damage a game folder:
//!
//! - A backup that has gone missing means our file **stays**. Deleting it
//!   would leave the game with no runtime at all, which is worse than leaving
//!   it with a working one it did not ship.
//! - A file that is no longer the one we installed - a game update overwrote
//!   it - is **left alone**. The game has moved on, and writing a months-old
//!   backup over a freshly patched file would be the restore causing the
//!   damage it exists to undo.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Code, Result};
use crate::fsx::atomic::copy_atomic;
use crate::fsx::paths::safe_path;
use crate::install::manifest::{self, InstallManifest, ManifestFile};
use crate::jobs::Cancel;

pub struct Request<'a> {
    pub game_dir: &'a Path,
    pub manifest_root: &'a Path,
    pub cancel: &'a Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Action {
    /// The game's own file is back.
    RestoredOriginal,
    /// We had added a file that was not there before, so undoing means
    /// removing it.
    RemovedOurs,
    /// Left as it is, on purpose. `detail` says why.
    LeftAlone,
    /// Could not be completed. `detail` says what went wrong.
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileOutcome {
    pub rel: String,
    pub action: Action,
    pub detail: String,
    /// The machine code, for a `Failed`. The UI leads with its own text for
    /// the code and uses `detail` for the specifics, the same way the
    /// preflight checks work.
    pub code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "outcome")]
pub enum Outcome {
    /// Nothing was ever installed here by us.
    NothingInstalled,
    Restored(Report),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub files: Vec<FileOutcome>,
    /// True when every recorded file was dealt with and the record has been
    /// cleared. False means the manifest was kept so a later run can finish.
    pub complete: bool,
}

/// What a restore *would* do, without doing it.
///
/// The same courtesy the install plan extends: a user about to undo something
/// should be able to see that two files will come back, one will be removed and
/// one will be left alone because the game has since been patched.
pub fn preview(request: &Request<'_>) -> Result<Outcome> {
    let Some(record) = manifest::load(request.manifest_root, request.game_dir)? else {
        return Ok(Outcome::NothingInstalled);
    };
    let files = record
        .files
        .iter()
        .map(|file| decide(&record, file))
        .collect();
    Ok(Outcome::Restored(Report {
        files,
        complete: false,
    }))
}

pub fn restore(request: &Request<'_>) -> Result<Outcome> {
    let Some(record) = manifest::load(request.manifest_root, request.game_dir)? else {
        return Ok(Outcome::NothingInstalled);
    };

    let mut files: Vec<FileOutcome> = Vec::new();
    for file in &record.files {
        if request.cancel.is_cancelled() {
            // Stopping is safe: what has been done is done correctly, and
            // running again picks up the rest. The manifest is kept.
            files.push(FileOutcome {
                rel: file.rel.clone(),
                action: Action::LeftAlone,
                detail: "cancelled before this file".to_owned(),
                code: Some(Code::JobCancelled.as_str().to_owned()),
            });
            continue;
        }
        files.push(act(&record, file));
    }

    // Only clear the record once nothing is outstanding. A manifest that
    // survives is what makes a second attempt possible.
    // Keyed on the code rather than on the wording of `detail`, which is
    // diagnostic text and free to change.
    let outstanding = files.iter().any(|file| {
        file.action == Action::Failed || file.code.as_deref() == Some(Code::JobCancelled.as_str())
    });
    if !outstanding {
        cleanup(&record, request.manifest_root)?;
    }

    Ok(Outcome::Restored(Report {
        complete: !outstanding,
        files,
    }))
}

fn failed(rel: &str, error: &crate::Error) -> FileOutcome {
    FileOutcome {
        rel: rel.to_owned(),
        action: Action::Failed,
        detail: error.detail.clone(),
        code: Some(error.code.as_str().to_owned()),
    }
}

/// The decision for one file, shared by `preview` and `restore` so the two
/// cannot disagree about what is going to happen.
fn decide(record: &InstallManifest, file: &ManifestFile) -> FileOutcome {
    let leave = |detail: &str| FileOutcome {
        rel: file.rel.clone(),
        action: Action::LeftAlone,
        detail: detail.to_owned(),
        code: None,
    };

    let target = match safe_path(&record.game_dir, &file.rel) {
        Ok(path) => path,
        Err(error) => return failed(&file.rel, &error),
    };

    if !target.exists() {
        // Already gone. If there is an original to put back, put it back;
        // otherwise there is nothing left to do.
        return match file.replaced.as_ref() {
            Some(_) => FileOutcome {
                rel: file.rel.clone(),
                action: Action::RestoredOriginal,
                detail: "our file is already gone; the original goes back".to_owned(),
                code: None,
            },
            None => leave("already removed"),
        };
    }

    match crate::hash::hash_file(&target) {
        Ok(found) if !crate::hash::matches(&found, &file.sha256) => {
            return leave(
                "this is no longer the file we installed - most likely a game \
                 update replaced it, so it is left as it is",
            )
        }
        Err(error) => return failed(&file.rel, &error),
        Ok(_) => {}
    }

    match file.replaced.as_ref() {
        Some(original) if original.backup.is_file() => FileOutcome {
            rel: file.rel.clone(),
            action: Action::RestoredOriginal,
            detail: original
                .version
                .clone()
                .unwrap_or_else(|| "the original file".to_owned()),
            code: None,
        },
        Some(_) => leave(
            "the saved copy of the original is missing, so ours is left in \
             place - removing it would leave the game with no runtime at all",
        ),
        None => FileOutcome {
            rel: file.rel.clone(),
            action: Action::RemovedOurs,
            detail: "we added this file; it was not there before".to_owned(),
            code: None,
        },
    }
}

fn act(record: &InstallManifest, file: &ManifestFile) -> FileOutcome {
    let planned = decide(record, file);
    match planned.action {
        Action::RestoredOriginal => match put_back(record, file) {
            Ok(()) => planned,
            Err(error) => failed(&file.rel, &error),
        },
        Action::RemovedOurs => {
            let target = record.game_dir.join(file.rel.replace('\\', "/"));
            match std::fs::remove_file(&target) {
                Ok(()) => planned,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => planned,
                Err(error) => failed(
                    &file.rel,
                    &crate::Error::new(
                        Code::StateUnwritable,
                        format!("could not remove {}: {error}", target.display()),
                    ),
                ),
            }
        }
        Action::LeftAlone | Action::Failed => planned,
    }
}

fn put_back(record: &InstallManifest, file: &ManifestFile) -> Result<()> {
    let target = safe_path(&record.game_dir, &file.rel)?;
    let Some(original) = file.replaced.as_ref() else {
        return crate::error::fail(
            Code::StateCorrupt,
            format!("{} has no recorded original", file.rel),
        );
    };

    // Verify before writing. A corrupted backup written over a working file
    // would turn an undo into the very damage it is meant to reverse.
    let found = crate::hash::hash_file(&original.backup)?;
    crate::hash::verify(&original.backup, &found, &original.sha256)?;
    copy_atomic(&original.backup, &target)
}

/// Remove the backups and the record, in that order.
///
/// Backups first: a manifest with no backups is recoverable (there is nothing
/// left to restore, which is the truth), whereas backups with no manifest are
/// orphaned bytes nothing will ever clean up.
fn cleanup(record: &InstallManifest, manifest_root: &Path) -> Result<()> {
    for file in &record.files {
        if let Some(original) = file.replaced.as_ref() {
            let _ = std::fs::remove_file(&original.backup);
            // The per-install directory, once its contents have gone. Fails
            // harmlessly while anything remains in it.
            if let Some(parent) = original.backup.parent() {
                let _ = std::fs::remove_dir(parent);
            }
        }
    }
    manifest::remove(manifest_root, &record.game_dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install::apply::{self, Applied};
    use crate::install::plan::{build_plan, PackageFile, Plan, PlanInput, PresentFile, Route};
    use crate::scan::folder::RuntimeKind;
    use std::path::PathBuf;

    struct Bench {
        _root: tempfile::TempDir,
        game: PathBuf,
        source: PathBuf,
        journals: PathBuf,
        backups: PathBuf,
        manifests: PathBuf,
        cancel: Cancel,
    }

    fn bench() -> Bench {
        let root = tempfile::tempdir().expect("tempdir");
        let game = root.path().join("game");
        let source = root.path().join("package");
        std::fs::create_dir_all(game.join("bin/x64")).expect("game dirs");
        std::fs::create_dir_all(&source).expect("source dir");
        Bench {
            game,
            source,
            journals: root.path().join("journal"),
            backups: root.path().join("backups"),
            manifests: root.path().join("installs"),
            cancel: Cancel::new(),
            _root: root,
        }
    }

    impl Bench {
        fn offer(&self, name: &str, bytes: &[u8]) -> PackageFile {
            std::fs::write(self.source.join(name), bytes).expect("write source");
            PackageFile {
                name: name.to_owned(),
                kind: RuntimeKind::Dlss,
                version: Some("310.8.0.0".to_owned()),
                size: bytes.len() as u64,
                sha256: crate::hash::hash_bytes(bytes),
            }
        }

        fn existing(&self, rel: &str, bytes: &[u8]) -> PresentFile {
            let path = self.game.join(rel);
            std::fs::write(&path, bytes).expect("write existing");
            PresentFile {
                rel: rel.to_owned(),
                kind: RuntimeKind::Dlss,
                version: Some("310.1.0.0".to_owned()),
                size: bytes.len() as u64,
                sha256: crate::hash::hash_bytes(bytes),
                managed: false,
            }
        }

        fn install(&self, present: Vec<PresentFile>, pkg: Vec<PackageFile>) -> Applied {
            let plan: Plan = build_plan(&PlanInput {
                route: Route::NativeDll,
                install_dir: "bin/x64".to_owned(),
                present,
                pkg,
            })
            .expect("plan");
            match apply::apply(&apply::Request {
                game_dir: &self.game,
                plan: &plan,
                source_dir: &self.source,
                journal_root: &self.journals,
                backup_root: &self.backups,
                manifest_root: &self.manifests,
                cancel: &self.cancel,
            })
            .expect("apply")
            {
                apply::Outcome::Installed(applied) => applied,
                other => panic!("install failed: {other:?}"),
            }
        }

        fn request(&self) -> Request<'_> {
            Request {
                game_dir: &self.game,
                manifest_root: &self.manifests,
                cancel: &self.cancel,
            }
        }

        fn undo(&self) -> Report {
            match restore(&self.request()).expect("restore") {
                Outcome::Restored(report) => report,
                Outcome::NothingInstalled => panic!("expected something installed"),
            }
        }

        fn target(&self, rel: &str) -> PathBuf {
            self.game.join(rel)
        }
    }

    /// Read by the frontend, so the tagged shape is part of the contract.
    /// `NothingInstalled` is a unit variant and serialises to the tag alone,
    /// which is the case a consumer is most likely to get wrong.
    #[test]
    fn the_outcome_serialises_as_a_tagged_object() {
        let nothing = serde_json::to_value(Outcome::NothingInstalled).expect("serialise");
        assert_eq!(nothing["outcome"], "nothingInstalled");

        let restored = serde_json::to_value(Outcome::Restored(Report {
            files: vec![FileOutcome {
                rel: "bin/x64/a.dll".to_owned(),
                action: Action::RestoredOriginal,
                detail: "310.1.0.0".to_owned(),
                code: None,
            }],
            complete: true,
        }))
        .expect("serialise");
        assert_eq!(restored["outcome"], "restored");
        assert_eq!(restored["complete"], true);
        assert_eq!(restored["files"][0]["action"], "restoredOriginal");
    }

    #[test]
    fn nothing_installed_is_not_an_error() {
        let bench = bench();
        assert_eq!(
            restore(&bench.request()).expect("restore"),
            Outcome::NothingInstalled
        );
    }

    #[test]
    fn a_replaced_file_comes_back_exactly() {
        let bench = bench();
        let present = bench.existing("bin/x64/nvngx_dlss.dll", b"the game's own");
        bench.install(
            vec![present],
            vec![bench.offer("nvngx_dlss.dll", b"our runtime")],
        );

        let report = bench.undo();
        assert!(report.complete);
        assert_eq!(report.files[0].action, Action::RestoredOriginal);
        assert_eq!(
            std::fs::read(bench.target("bin/x64/nvngx_dlss.dll")).expect("read"),
            b"the game's own"
        );
        // The record is cleared, so a second restore has nothing to do.
        assert_eq!(
            restore(&bench.request()).expect("restore"),
            Outcome::NothingInstalled
        );
    }

    #[test]
    fn a_file_we_added_is_removed_rather_than_restored() {
        let bench = bench();
        bench.install(vec![], vec![bench.offer("sl.dlss_g.dll", b"added by us")]);

        let report = bench.undo();
        assert!(report.complete);
        assert_eq!(report.files[0].action, Action::RemovedOurs);
        assert!(!bench.target("bin/x64/sl.dlss_g.dll").exists());
    }

    #[test]
    fn a_file_a_game_update_has_since_replaced_is_left_alone() {
        // Writing a months-old backup over a freshly patched file would be the
        // restore causing the damage it exists to undo.
        let bench = bench();
        let present = bench.existing("bin/x64/nvngx_dlss.dll", b"the game's own");
        bench.install(
            vec![present],
            vec![bench.offer("nvngx_dlss.dll", b"our runtime")],
        );
        std::fs::write(
            bench.target("bin/x64/nvngx_dlss.dll"),
            b"what the patch installed",
        )
        .expect("simulate a patch");

        let report = bench.undo();
        assert_eq!(report.files[0].action, Action::LeftAlone);
        assert!(report.files[0].detail.contains("game update"));
        assert_eq!(
            std::fs::read(bench.target("bin/x64/nvngx_dlss.dll")).expect("read"),
            b"what the patch installed"
        );
        // Left alone is a finished outcome, not an outstanding one.
        assert!(report.complete);
    }

    #[test]
    fn a_missing_backup_leaves_our_file_in_place() {
        // Removing it would leave the game with no runtime at all.
        let bench = bench();
        let present = bench.existing("bin/x64/nvngx_dlss.dll", b"the game's own");
        bench.install(
            vec![present],
            vec![bench.offer("nvngx_dlss.dll", b"our runtime")],
        );

        let record = manifest::load(&bench.manifests, &bench.game)
            .expect("load")
            .expect("manifest");
        let backup = record.files[0]
            .replaced
            .as_ref()
            .expect("a recorded original")
            .backup
            .clone();
        std::fs::remove_file(&backup).expect("lose the backup");

        let report = bench.undo();
        assert_eq!(report.files[0].action, Action::LeftAlone);
        assert!(report.files[0].detail.contains("no runtime at all"));
        assert_eq!(
            std::fs::read(bench.target("bin/x64/nvngx_dlss.dll")).expect("read"),
            b"our runtime"
        );
    }

    #[test]
    fn a_corrupted_backup_is_refused_and_the_record_kept() {
        let bench = bench();
        let present = bench.existing("bin/x64/nvngx_dlss.dll", b"the game's own");
        bench.install(
            vec![present],
            vec![bench.offer("nvngx_dlss.dll", b"our runtime")],
        );

        let record = manifest::load(&bench.manifests, &bench.game)
            .expect("load")
            .expect("manifest");
        let backup = record.files[0]
            .replaced
            .as_ref()
            .expect("a recorded original")
            .backup
            .clone();
        std::fs::write(&backup, b"corrupted on disk").expect("corrupt it");

        let report = bench.undo();
        assert_eq!(report.files[0].action, Action::Failed);
        assert_eq!(report.files[0].code.as_deref(), Some("verifyFailed"));
        assert!(!report.complete);
        // Ours is still there, untouched, and the record survives so a later
        // attempt is possible.
        assert_eq!(
            std::fs::read(bench.target("bin/x64/nvngx_dlss.dll")).expect("read"),
            b"our runtime"
        );
        assert!(manifest::load(&bench.manifests, &bench.game)
            .expect("load")
            .is_some());
    }

    #[test]
    fn a_restore_can_be_run_again_after_being_interrupted() {
        let bench = bench();
        let first = bench.existing("bin/x64/a.dll", b"original a");
        let second = bench.existing("bin/x64/b.dll", b"original b");
        bench.install(
            vec![first, second],
            vec![
                bench.offer("a.dll", b"ours a"),
                bench.offer("b.dll", b"ours b"),
            ],
        );

        // Cancelled: nothing is attempted, and the record is kept.
        bench.cancel.cancel();
        let stopped = bench.undo();
        assert!(!stopped.complete);
        assert!(stopped
            .files
            .iter()
            .all(|file| file.code.as_deref() == Some("jobCancelled")));

        // A fresh run finishes the job.
        let resumed = Request {
            game_dir: &bench.game,
            manifest_root: &bench.manifests,
            cancel: &Cancel::new(),
        };
        match restore(&resumed).expect("restore") {
            Outcome::Restored(report) => assert!(report.complete, "{report:?}"),
            other => panic!("expected a restore, got {other:?}"),
        }
        assert_eq!(
            std::fs::read(bench.target("bin/x64/a.dll")).expect("read a"),
            b"original a"
        );
        assert_eq!(
            std::fs::read(bench.target("bin/x64/b.dll")).expect("read b"),
            b"original b"
        );
    }

    #[test]
    fn restoring_twice_is_idempotent() {
        // The property that lets a restore go without a journal: repeating it
        // is how an interrupted one is repaired.
        let bench = bench();
        let present = bench.existing("bin/x64/nvngx_dlss.dll", b"the game's own");
        bench.install(
            vec![present],
            vec![bench.offer("nvngx_dlss.dll", b"our runtime")],
        );
        assert!(bench.undo().complete);
        assert_eq!(
            restore(&bench.request()).expect("restore"),
            Outcome::NothingInstalled
        );
        assert_eq!(
            std::fs::read(bench.target("bin/x64/nvngx_dlss.dll")).expect("read"),
            b"the game's own"
        );
    }

    #[test]
    fn a_preview_says_what_a_restore_would_do_without_doing_it() {
        let bench = bench();
        let present = bench.existing("bin/x64/nvngx_dlss.dll", b"the game's own");
        bench.install(
            vec![present],
            vec![
                bench.offer("nvngx_dlss.dll", b"our runtime"),
                bench.offer("sl.dlss_g.dll", b"added by us"),
            ],
        );

        let previewed = match preview(&bench.request()).expect("preview") {
            Outcome::Restored(report) => report,
            other => panic!("expected a preview, got {other:?}"),
        };
        let action_for = |rel: &str| {
            previewed
                .files
                .iter()
                .find(|file| file.rel == rel)
                .map(|file| file.action)
                .expect("a file")
        };
        assert_eq!(
            action_for("bin/x64/nvngx_dlss.dll"),
            Action::RestoredOriginal
        );
        assert_eq!(action_for("bin/x64/sl.dlss_g.dll"), Action::RemovedOurs);

        // And nothing actually changed.
        assert_eq!(
            std::fs::read(bench.target("bin/x64/nvngx_dlss.dll")).expect("read"),
            b"our runtime"
        );
        assert!(bench.target("bin/x64/sl.dlss_g.dll").exists());
        assert!(manifest::load(&bench.manifests, &bench.game)
            .expect("load")
            .is_some());
    }

    #[test]
    fn the_backup_store_is_cleared_once_the_originals_are_back() {
        let bench = bench();
        let present = bench.existing("bin/x64/nvngx_dlss.dll", b"the game's own");
        bench.install(
            vec![present],
            vec![bench.offer("nvngx_dlss.dll", b"our runtime")],
        );
        let record = manifest::load(&bench.manifests, &bench.game)
            .expect("load")
            .expect("manifest");
        let backup = record.files[0]
            .replaced
            .as_ref()
            .expect("original")
            .backup
            .clone();
        assert!(backup.is_file());

        assert!(bench.undo().complete);
        assert!(!backup.exists(), "the spare copy should be cleared");
    }
}
