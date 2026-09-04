//! Carrying out a plan.
//!
//! This is the only code in the project that writes into a game folder, and it
//! is deliberately the least clever. It makes no decisions: the plan already
//! decided, purely and reproducibly, and this walks it. Anything that looks
//! like a judgement call here would be a judgement the user was never shown.
//!
//! The order is the whole design:
//!
//! 1. **Check everything first.** Preflight, then every path resolved against
//!    the filesystem, then every source file hashed. Not one byte of the game
//!    folder is touched until all of it has passed - the same discipline the
//!    archive extractor uses, and for the same reason: a refusal half-way
//!    through leaves a folder in a state nobody described.
//! 2. **Write the intent down.** The journal is fsynced before the first
//!    target file is opened.
//! 3. **Then act**, one file at a time: copy the original aside, replace it,
//!    verify what landed, record the step. A crash at any instant is
//!    recoverable, because the intent and the progress are both on the platter.
//! 4. **Commit, then record.** The manifest is written after the commit
//!    marker, so a manifest never claims an install that did not finish.
//!
//! A failure at any point rolls back through the journal rather than leaving
//! the folder as it lies, and it rolls back through exactly the same code path
//! that recovery-after-a-crash uses, so the two cannot drift apart.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{fail, Code, Result};
use crate::fsx::atomic::copy_atomic;
use crate::fsx::paths::safe_path;
use crate::install::journal::{self, Journal, JournalRecord, JournalStep, JOURNAL_VERSION};
use crate::install::manifest::{self, InstallManifest, ManifestFile, Replaced};
use crate::install::plan::{Plan, Step, StepAction};
use crate::install::preflight::{self, Preflight};
use crate::jobs::Cancel;

pub struct Request<'a> {
    pub game_dir: &'a Path,
    pub plan: &'a Plan,
    /// Directory holding the package's files, named as the plan's steps name
    /// them. Usually an extracted archive.
    pub source_dir: &'a Path,
    /// Where journals live. One directory for the whole application.
    pub journal_root: &'a Path,
    /// Where displaced originals are kept, permanently.
    pub backup_root: &'a Path,
    /// Where per-game install records are kept.
    pub manifest_root: &'a Path,
    /// The hardware generation this package's runtime needs, if it states one.
    /// Passed straight to the preflight, which decides what to do about it.
    pub requires: Option<crate::platform::gpu::Generation>,
    pub cancel: &'a Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Applied {
    pub journal_id: String,
    /// Files written, in the order they were written.
    pub installed: Vec<String>,
    /// Files the plan said were already correct.
    pub skipped: Vec<String>,
    pub bytes_written: u64,
}

/// How far an install got before it failed.
///
/// The only question a user actually has after a failure is "what state is my
/// game folder in?", so that is what this answers, rather than making them
/// infer it from an error code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Reached {
    /// Failed before the first write. The folder was never touched, so there
    /// was nothing to undo - not the same claim as having undone something.
    NothingWritten,
    /// Files were written and have all been put back as they were.
    RolledBack,
    /// Files were written and the undo could not finish. This is the one state
    /// that needs the user told loudly; the journal is kept and the next
    /// launch will try again.
    PartiallyApplied,
}

/// Everything known about a failed install.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Failure {
    pub code: String,
    pub message: String,
    pub reached: Reached,
    /// Why the rollback could not finish, when it could not.
    pub rollback_failures: Vec<String>,
}

/// The outcome of an install attempt.
///
/// A refused preflight is returned rather than raised, because it is not an
/// error in the "something went wrong" sense - it is the checks doing their
/// job, and the user needs the whole report to act on, not one code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "outcome")]
pub enum Outcome {
    Installed(Applied),
    Refused(Preflight),
    Failed(Failure),
}

/// One resolved step: the plan's intent, plus the absolute paths and the hash
/// of what is being displaced. Everything gathered before any write.
struct Resolved<'a> {
    step: &'a Step,
    target: PathBuf,
    source: PathBuf,
    backup: Option<PathBuf>,
    replaced_sha256: Option<String>,
    replaced_size: u64,
    replaced_version: Option<String>,
}

pub fn apply(request: &Request<'_>) -> Result<Outcome> {
    let report = preflight::preflight(&preflight::Request {
        game_dir: request.game_dir,
        plan: request.plan,
        source_dir: request.source_dir,
        backup_dir: request.backup_root,
        requires: request.requires,
    });
    if !report.ok {
        return Ok(Outcome::Refused(report));
    }

    // Nothing to do is a success, and must not create a journal or a backup
    // directory. Re-running an install should be free.
    if request.plan.changes == 0 {
        return Ok(Outcome::Installed(Applied {
            journal_id: String::new(),
            installed: Vec::new(),
            skipped: skipped(request.plan),
            bytes_written: 0,
        }));
    }

    let id = journal::new_id();
    let backup_dir = request
        .backup_root
        .join(manifest::key_for(request.game_dir))
        .join(&id);

    // Resolution touches nothing, so a failure here means the folder is
    // exactly as it was found and there is no journal to clean up.
    let resolved = match resolve(request, &backup_dir) {
        Ok(resolved) => resolved,
        Err(error) => {
            return Ok(Outcome::Failed(Failure {
                code: error.code.as_str().to_owned(),
                message: error.detail,
                reached: Reached::NothingWritten,
                rollback_failures: Vec::new(),
            }))
        }
    };

    // Intent first. After this line a crash is recoverable; before it, there
    // is nothing to recover because nothing has been touched.
    let record = JournalRecord {
        version: JOURNAL_VERSION,
        id: id.clone(),
        game_dir: request.game_dir.to_path_buf(),
        route: request.plan.route,
        created_at: now_millis(),
        steps: resolved
            .iter()
            .enumerate()
            .map(|(index, item)| JournalStep {
                index,
                rel: item.step.rel.clone(),
                action: item.step.action,
                expected_sha256: item.step.sha256.clone(),
                expected_size: item.step.write_bytes,
                backup: item.backup.clone(),
                replaced_sha256: item.replaced_sha256.clone(),
            })
            .collect(),
    };
    let mut journal = Journal::begin(request.journal_root, record)?;

    match write_all(request, &resolved, &mut journal, &backup_dir) {
        Ok(installed) => {
            journal.commit()?;
            // The manifest is written after the commit marker on purpose: a
            // manifest that claims an install which did not finish is worse
            // than no manifest, because it is what an uninstall trusts.
            manifest::save(request.manifest_root, &built_manifest(request, &resolved))?;
            journal.remove()?;
            Ok(Outcome::Installed(Applied {
                journal_id: id,
                bytes_written: installed
                    .iter()
                    .filter_map(|rel| {
                        resolved
                            .iter()
                            .find(|item| &item.step.rel == rel)
                            .map(|item| item.step.write_bytes)
                    })
                    .sum(),
                installed,
                skipped: skipped(request.plan),
            }))
        }
        Err(error) => {
            // Undo through the journal, using the same path a crash would.
            let undone = journal::recover_dir(journal.dir());
            let reached = if undone.failures.is_empty() {
                Reached::RolledBack
            } else {
                Reached::PartiallyApplied
            };
            Ok(Outcome::Failed(Failure {
                code: error.code.as_str().to_owned(),
                message: error.detail,
                reached,
                rollback_failures: undone.failures,
            }))
        }
    }
}

/// Resolve and verify everything, touching nothing.
///
/// Every failure this can raise happens before the first write, which is what
/// makes a refusal harmless.
fn resolve<'a>(request: &Request<'a>, backup_dir: &Path) -> Result<Vec<Resolved<'a>>> {
    let mut resolved = Vec::new();
    for (index, step) in request
        .plan
        .steps
        .iter()
        .filter(|step| step.action != StepAction::Skip)
        .enumerate()
    {
        // The filesystem check, as opposed to the plan's lexical one: this is
        // where a junction pointing out of the game folder is caught.
        let target = safe_path(request.game_dir, &step.rel)?;

        let Some(name) = step
            .rel
            .rsplit(['/', '\\'])
            .next()
            .filter(|n| !n.is_empty())
        else {
            return fail(
                Code::PackageInvalid,
                format!("step has no file name: {}", step.rel),
            );
        };
        let source = request.source_dir.join(name);

        // The package must be the package the plan was built from. If the
        // archive was re-extracted, or swapped underneath us, the plan's
        // decisions no longer describe these bytes.
        let actual = crate::hash::hash_file(&source)?;
        if !crate::hash::matches(&actual, &step.sha256) {
            return fail(
                Code::PlanStale,
                format!("{name} is not the file this plan was built from - re-scan and plan again"),
            );
        }

        // What is about to be displaced, hashed now so the rollback can verify
        // the backup before writing it back.
        let (replaced_sha256, replaced_size) = if target.is_file() {
            let hash = crate::hash::hash_file(&target)?;
            let size = std::fs::metadata(&target)
                .map(|meta| meta.len())
                .unwrap_or(0);
            (Some(hash), size)
        } else {
            (None, 0)
        };

        // A `Replace` whose target has vanished since the plan was built, or a
        // `Create` whose target has appeared, means the folder changed under
        // us. Either could be a game update mid-plan.
        match (step.action, replaced_sha256.is_some()) {
            (StepAction::Replace, false) => {
                return fail(
                    Code::PlanStale,
                    format!(
                        "{} was there when this was planned and is not now",
                        step.rel
                    ),
                )
            }
            (StepAction::Create, true) => {
                return fail(
                    Code::PlanStale,
                    format!("{} has appeared since this was planned", step.rel),
                )
            }
            _ => {}
        }

        resolved.push(Resolved {
            step,
            target,
            source,
            // Numbered rather than named after the game's file: two runtimes
            // in different folders can share a file name, and a flat numbered
            // store cannot collide or reproduce an awkward path.
            backup: replaced_sha256
                .as_ref()
                .map(|_| backup_dir.join(format!("{index:04}.bin"))),
            replaced_sha256,
            replaced_size,
            replaced_version: step.from_version.clone(),
        });
    }
    Ok(resolved)
}

/// Walk the resolved steps, writing. Any error here is rolled back by the
/// caller.
fn write_all(
    request: &Request<'_>,
    resolved: &[Resolved<'_>],
    journal: &mut Journal,
    backup_dir: &Path,
) -> Result<Vec<String>> {
    let mut installed = Vec::new();

    for (index, item) in resolved.iter().enumerate() {
        // Checked between files rather than inside a copy: a half-copied DLL is
        // not a state worth stopping in, and the atomic write means a whole
        // file either lands or does not.
        if request.cancel.is_cancelled() {
            return fail(Code::JobCancelled, "cancelled before the next file");
        }

        // Copy the original aside first. Until this succeeds, the replacement
        // must not begin - the backup is the only route back.
        if let Some(backup) = item.backup.as_ref() {
            std::fs::create_dir_all(backup_dir).map_err(|error| {
                crate::Error::new(
                    Code::StateUnwritable,
                    format!(
                        "could not create the backup folder {}: {error}",
                        backup_dir.display()
                    ),
                )
            })?;
            copy_atomic(&item.target, backup)?;
            // Verify the copy before trusting it with the only surviving
            // version of the user's file.
            if let Some(expected) = item.replaced_sha256.as_ref() {
                let actual = crate::hash::hash_file(backup)?;
                crate::hash::verify(backup, &actual, expected)?;
            }
        }

        copy_atomic(&item.source, &item.target)?;

        // Verify what actually landed, not what we sent. This is the check
        // that catches a short write, a full disk that reported success, or
        // something else writing to the same path at the same moment.
        let written = crate::hash::hash_file(&item.target)?;
        crate::hash::verify(&item.target, &written, &item.step.sha256)?;

        journal.note_applied(index)?;
        installed.push(item.step.rel.clone());
    }

    Ok(installed)
}

fn built_manifest(request: &Request<'_>, resolved: &[Resolved<'_>]) -> InstallManifest {
    InstallManifest {
        version: manifest::MANIFEST_VERSION,
        game_dir: request.game_dir.to_path_buf(),
        route: request.plan.route,
        installed_at: now_millis(),
        files: resolved
            .iter()
            .map(|item| ManifestFile {
                rel: item.step.rel.clone(),
                kind: item.step.kind,
                sha256: item.step.sha256.clone(),
                size: item.step.write_bytes,
                version: item.step.to_version.clone(),
                replaced: item.replaced_sha256.as_ref().and_then(|hash| {
                    item.backup.as_ref().map(|backup| Replaced {
                        sha256: hash.clone(),
                        size: item.replaced_size,
                        version: item.replaced_version.clone(),
                        backup: backup.clone(),
                    })
                }),
            })
            .collect(),
    }
}

fn skipped(plan: &Plan) -> Vec<String> {
    plan.steps
        .iter()
        .filter(|step| step.action == StepAction::Skip)
        .map(|step| step.rel.clone())
        .collect()
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install::manifest::FileStatus;
    use crate::install::plan::{build_plan, PackageFile, PlanInput, PresentFile, Route};
    use crate::scan::folder::RuntimeKind;

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

        fn existing(&self, rel: &str, bytes: &[u8], version: &str) -> PresentFile {
            let path = self.game.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("dirs");
            }
            std::fs::write(&path, bytes).expect("write existing");
            PresentFile {
                rel: rel.to_owned(),
                kind: RuntimeKind::Dlss,
                version: Some(version.to_owned()),
                size: bytes.len() as u64,
                sha256: crate::hash::hash_bytes(bytes),
                managed: false,
            }
        }

        fn plan(&self, present: Vec<PresentFile>, pkg: Vec<PackageFile>) -> Plan {
            build_plan(&PlanInput {
                route: Route::NativeDll,
                install_dir: "bin/x64".to_owned(),
                present,
                pkg,
            })
            .expect("plan")
        }

        fn run(&self, plan: &Plan) -> Outcome {
            apply(&Request {
                game_dir: &self.game,
                plan,
                source_dir: &self.source,
                journal_root: &self.journals,
                backup_root: &self.backups,
                manifest_root: &self.manifests,
                requires: None,
                cancel: &self.cancel,
            })
            .expect("apply")
        }

        fn target(&self, rel: &str) -> PathBuf {
            self.game.join(rel)
        }

        fn journals_left(&self) -> usize {
            journal::survey(&self.journals).expect("survey").len()
        }
    }

    fn installed(outcome: Outcome) -> Applied {
        match outcome {
            Outcome::Installed(applied) => applied,
            other => panic!("expected an install, got {other:?}"),
        }
    }

    /// The frontend reads these shapes, so the tag-plus-flattened-fields
    /// layout that `#[serde(tag = "outcome")]` produces is part of the
    /// contract rather than an implementation detail. An internally tagged
    /// enum silently fails on a newtype variant wrapping a non-map, so this
    /// also proves the three variants stay map-shaped.
    #[test]
    fn the_outcome_serialises_as_a_tagged_object() {
        let installed = serde_json::to_value(Outcome::Installed(Applied {
            journal_id: "j1".to_owned(),
            installed: vec!["bin/x64/a.dll".to_owned()],
            skipped: Vec::new(),
            bytes_written: 12,
        }))
        .expect("serialise");
        assert_eq!(installed["outcome"], "installed");
        assert_eq!(installed["journalId"], "j1");
        assert_eq!(installed["bytesWritten"], 12);

        let failed = serde_json::to_value(Outcome::Failed(Failure {
            code: "planStale".to_owned(),
            message: "changed underneath".to_owned(),
            reached: Reached::NothingWritten,
            rollback_failures: Vec::new(),
        }))
        .expect("serialise");
        assert_eq!(failed["outcome"], "failed");
        assert_eq!(failed["reached"], "nothingWritten");

        let refused = serde_json::to_value(Outcome::Refused(Preflight {
            checks: Vec::new(),
            ok: false,
        }))
        .expect("serialise");
        assert_eq!(refused["outcome"], "refused");
        assert_eq!(refused["ok"], false);
    }

    #[test]
    fn a_fresh_install_writes_the_file_and_records_it() {
        let bench = bench();
        let plan = bench.plan(
            vec![],
            vec![bench.offer("nvngx_dlss.dll", b"the new runtime")],
        );
        let applied = installed(bench.run(&plan));

        assert_eq!(applied.installed, vec!["bin/x64/nvngx_dlss.dll"]);
        assert_eq!(
            std::fs::read(bench.target("bin/x64/nvngx_dlss.dll")).expect("read"),
            b"the new runtime"
        );
        // The journal is bookkeeping and goes away once the install commits.
        assert_eq!(bench.journals_left(), 0);

        let record = manifest::load(&bench.manifests, &bench.game)
            .expect("load")
            .expect("a manifest");
        assert_eq!(record.files.len(), 1);
        assert!(record.files[0].replaced.is_none(), "nothing was displaced");
        assert!(manifest::verify(&record).intact);
    }

    #[test]
    fn a_replacement_keeps_the_original_and_can_be_verified() {
        let bench = bench();
        let present = bench.existing("bin/x64/nvngx_dlss.dll", b"the game's own", "310.1.0.0");
        let plan = bench.plan(
            vec![present],
            vec![bench.offer("nvngx_dlss.dll", b"the new runtime")],
        );
        installed(bench.run(&plan));

        assert_eq!(
            std::fs::read(bench.target("bin/x64/nvngx_dlss.dll")).expect("read"),
            b"the new runtime"
        );

        let record = manifest::load(&bench.manifests, &bench.game)
            .expect("load")
            .expect("a manifest");
        let original = record.files[0]
            .replaced
            .as_ref()
            .expect("the displaced file is recorded");
        // The user's file still exists, outside the journal, months later.
        assert_eq!(
            std::fs::read(&original.backup).expect("read backup"),
            b"the game's own"
        );
        assert_eq!(original.version.as_deref(), Some("310.1.0.0"));
        assert!(manifest::verify(&record).intact);
    }

    #[test]
    fn an_install_with_nothing_to_do_writes_nothing_at_all() {
        let bench = bench();
        let bytes = b"already correct";
        let package = bench.offer("nvngx_dlss.dll", bytes);
        let present = bench.existing("bin/x64/nvngx_dlss.dll", bytes, "310.8.0.0");
        let plan = bench.plan(vec![present], vec![package]);
        assert_eq!(plan.changes, 0);

        let applied = installed(bench.run(&plan));
        assert!(applied.installed.is_empty());
        assert_eq!(applied.skipped, vec!["bin/x64/nvngx_dlss.dll"]);
        // No journal, no backup folder, no manifest: re-running is free.
        assert_eq!(bench.journals_left(), 0);
        assert!(!bench.backups.exists());
    }

    #[test]
    fn a_refused_preflight_returns_the_whole_report_rather_than_one_code() {
        let bench = bench();
        let plan = bench.plan(vec![], vec![bench.offer("nvngx_dlss.dll", b"bytes")]);
        // Remove the source after planning, so preflight has something to fail.
        std::fs::remove_file(bench.source.join("nvngx_dlss.dll")).expect("remove");

        match bench.run(&plan) {
            Outcome::Refused(report) => {
                assert!(!report.ok);
                assert_eq!(report.checks.len(), 9);
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
        assert_eq!(bench.journals_left(), 0);
    }

    #[test]
    fn a_package_that_changed_since_planning_is_refused_before_any_write() {
        let bench = bench();
        let present = bench.existing("bin/x64/nvngx_dlss.dll", b"the game's own", "310.1.0.0");
        let plan = bench.plan(
            vec![present],
            vec![bench.offer("nvngx_dlss.dll", b"the planned bytes")],
        );
        // Same length, different content: passes preflight's size check and is
        // caught by the hash.
        std::fs::write(bench.source.join("nvngx_dlss.dll"), b"OTHER bytes......")
            .expect("swap the package");

        match bench.run(&plan) {
            Outcome::Failed(failure) => {
                assert_eq!(failure.code, "planStale");
                // Caught during resolution, so there was never anything to
                // undo - which is a stronger guarantee than a successful undo.
                assert_eq!(failure.reached, Reached::NothingWritten);
            }
            other => panic!("expected a stale-plan failure, got {other:?}"),
        }
        // The game's file is untouched, because the refusal came first.
        assert_eq!(
            std::fs::read(bench.target("bin/x64/nvngx_dlss.dll")).expect("read"),
            b"the game's own"
        );
        assert_eq!(bench.journals_left(), 0);
    }

    #[test]
    fn a_target_that_vanished_since_planning_is_refused() {
        let bench = bench();
        let present = bench.existing("bin/x64/nvngx_dlss.dll", b"the game's own", "310.1.0.0");
        let plan = bench.plan(
            vec![present],
            vec![bench.offer("nvngx_dlss.dll", b"the new runtime")],
        );
        std::fs::remove_file(bench.target("bin/x64/nvngx_dlss.dll")).expect("remove");

        match bench.run(&plan) {
            Outcome::Failed(failure) => assert_eq!(failure.code, "planStale"),
            // Preflight may also catch this; either refusal is acceptable, as
            // long as nothing was written.
            Outcome::Refused(_) => {}
            other => panic!("expected a refusal, got {other:?}"),
        }
        assert_eq!(bench.journals_left(), 0);
    }

    #[test]
    fn a_failure_part_way_through_puts_every_file_back() {
        // The case the journal exists for. Three files, and the third cannot
        // be written because a directory is sitting where it should go.
        let bench = bench();
        let first = bench.existing("bin/x64/a.dll", b"original a", "310.1.0.0");
        let second = bench.existing("bin/x64/b.dll", b"original b", "310.1.0.0");
        // `c.dll` as a directory: the copy into it must fail.
        std::fs::create_dir_all(bench.target("bin/x64/c.dll")).expect("obstruct");

        let plan = bench.plan(
            vec![first, second],
            vec![
                bench.offer("a.dll", b"new a"),
                bench.offer("b.dll", b"new b"),
                bench.offer("c.dll", b"new c"),
            ],
        );

        match bench.run(&plan) {
            Outcome::Failed(failure) => {
                assert_eq!(
                    failure.reached,
                    Reached::RolledBack,
                    "rollback should have succeeded: {:?}",
                    failure.rollback_failures
                );
            }
            other => panic!("expected a failure, got {other:?}"),
        }

        // Both replaced files are back to exactly their original bytes.
        assert_eq!(
            std::fs::read(bench.target("bin/x64/a.dll")).expect("read a"),
            b"original a"
        );
        assert_eq!(
            std::fs::read(bench.target("bin/x64/b.dll")).expect("read b"),
            b"original b"
        );
        // Nothing left behind, and no manifest claiming an install.
        assert_eq!(bench.journals_left(), 0);
        assert!(manifest::load(&bench.manifests, &bench.game)
            .expect("load")
            .is_none());
    }

    #[test]
    fn cancelling_mid_install_rolls_back_rather_than_leaving_it_half_done() {
        let bench = bench();
        let first = bench.existing("bin/x64/a.dll", b"original a", "310.1.0.0");
        let plan = bench.plan(vec![first], vec![bench.offer("a.dll", b"new a")]);

        bench.cancel.cancel();
        match bench.run(&plan) {
            Outcome::Failed(failure) => {
                assert_eq!(failure.code, "jobCancelled");
                assert_eq!(failure.reached, Reached::RolledBack);
            }
            other => panic!("expected a cancellation, got {other:?}"),
        }
        assert_eq!(
            std::fs::read(bench.target("bin/x64/a.dll")).expect("read"),
            b"original a"
        );
        assert_eq!(bench.journals_left(), 0);
    }

    #[test]
    fn a_manifest_notices_a_game_update_overwriting_our_file() {
        // Why verification exists: a patch replaces the very files a swap
        // targets, which is how an upscaler swap silently reverts.
        let bench = bench();
        let plan = bench.plan(vec![], vec![bench.offer("nvngx_dlss.dll", b"our runtime")]);
        installed(bench.run(&plan));

        std::fs::write(
            bench.target("bin/x64/nvngx_dlss.dll"),
            b"what the patch put back",
        )
        .expect("simulate a game update");

        let record = manifest::load(&bench.manifests, &bench.game)
            .expect("load")
            .expect("a manifest");
        let report = manifest::verify(&record);
        assert!(!report.intact);
        assert_eq!(report.files[0].status, FileStatus::Changed);
    }

    #[test]
    fn installing_twice_is_a_no_op_the_second_time() {
        let bench = bench();
        let package = bench.offer("nvngx_dlss.dll", b"the new runtime");
        let first = bench.plan(vec![], vec![package.clone()]);
        installed(bench.run(&first));

        // Plan again against what is now on disk.
        let present = PresentFile {
            rel: "bin/x64/nvngx_dlss.dll".to_owned(),
            kind: RuntimeKind::Dlss,
            version: Some("310.8.0.0".to_owned()),
            size: package.size,
            sha256: package.sha256.clone(),
            managed: true,
        };
        let second = bench.plan(vec![present], vec![package]);
        assert_eq!(second.changes, 0);
        let applied = installed(bench.run(&second));
        assert!(applied.installed.is_empty());
    }

    #[test]
    fn a_target_reached_through_a_symlink_is_refused() {
        // Only meaningful where a link can actually be created; on Windows
        // that needs either developer mode or elevation, so the test skips
        // rather than failing on a machine that will not allow it.
        let bench = bench();
        let real = bench.game.join("elsewhere");
        std::fs::create_dir_all(&real).expect("dirs");
        let link = bench.game.join("bin/x64/linked");

        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_dir(&real, &link).is_ok();
        #[cfg(not(windows))]
        let made = std::os::unix::fs::symlink(&real, &link).is_ok();
        if !made {
            return;
        }

        let plan = build_plan(&PlanInput {
            route: Route::NativeDll,
            install_dir: "bin/x64/linked".to_owned(),
            present: vec![],
            pkg: vec![bench.offer("nvngx_dlss.dll", b"bytes")],
        })
        .expect("plan");

        match bench.run(&plan) {
            Outcome::Refused(report) => {
                let path_check = report
                    .checks
                    .iter()
                    .find(|check| check.name == preflight::CheckName::PathSafety)
                    .expect("a path-safety check");
                assert_eq!(path_check.code.as_deref(), Some("symlinkRefused"));
            }
            other => panic!("a symlinked target must be refused, got {other:?}"),
        }
    }
}
