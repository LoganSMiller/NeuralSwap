//! The write-ahead journal.
//!
//! NTFS offers no way to replace several files as one transaction. Transactional
//! NTFS existed and Microsoft deprecated it, advising against new use. So an
//! install that touches four DLLs genuinely has three instants at which a power
//! cut leaves a game folder half-changed, and no amount of care inside the copy
//! loop removes them.
//!
//! What can be done is to make the half-changed state *recoverable*: write the
//! intent down first, durably, then work through it leaving a durable trace of
//! how far we got. Then a later run can always answer "what was happening, and
//! what should happen now?" without guessing - see [`super::recover`], which
//! holds that decision as a pure function.
//!
//! Two things live in separate places on purpose:
//!
//! - the **journal** is bookkeeping, and is deleted the moment an install
//!   commits;
//! - the **backups** are the user's original files, and outlive the journal
//!   entirely, because the install manifest points at them so the game can be
//!   put back the way it was months later.
//!
//! Conflating the two would mean either losing the ability to restore, or
//! keeping journals around forever as a side effect of keeping backups.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{fail, Code, Result};
use crate::fsx::atomic::{write_atomic, write_json_atomic};
use crate::install::plan::{Route, StepAction};
use crate::install::recover::{decide_recovery, JournalState, Recovery, RecoveryDecision};

/// Bumped only if the on-disk shape changes incompatibly. A journal from a
/// newer build is quarantined rather than misread - the whole point of the
/// file is that its meaning is unambiguous.
pub const JOURNAL_VERSION: u32 = 1;

const PLAN_FILE: &str = "plan.json";
const PROGRESS_FILE: &str = "progress.jsonl";
const COMMIT_FILE: &str = "committed";

/// One file's worth of intent, with everything a rollback needs even if the
/// package that prompted it is long gone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JournalStep {
    pub index: usize,
    /// Relative to the game directory, as the plan stated it.
    pub rel: String,
    /// Only `Create` and `Replace` are journalled; a skip changes nothing.
    pub action: StepAction,
    pub expected_sha256: String,
    pub expected_size: u64,
    /// Absolute path of the copy taken before the target was replaced.
    /// `None` for a `Create`, which is undone by deletion.
    pub backup: Option<PathBuf>,
    /// What the displaced file hashed to. A restore verifies against this, so
    /// a corrupted backup is caught rather than written back over good bytes.
    pub replaced_sha256: Option<String>,
}

/// The intent record, fsynced before a single target file is opened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JournalRecord {
    pub version: u32,
    pub id: String,
    pub game_dir: PathBuf,
    pub route: Route,
    pub created_at: i64,
    pub steps: Vec<JournalStep>,
}

/// An open journal. Dropping this does **not** clean up: an abandoned journal
/// is exactly the evidence recovery needs, and destroying it in a destructor
/// would erase the record of the crash that made it interesting.
#[derive(Debug)]
pub struct Journal {
    dir: PathBuf,
    record: JournalRecord,
    applied: Vec<usize>,
}

impl Journal {
    /// Write the intent and return a handle. After this returns, a crash at
    /// any later instant is recoverable.
    pub fn begin(journal_root: &Path, record: JournalRecord) -> Result<Self> {
        let dir = journal_root.join(&record.id);
        fs::create_dir_all(&dir).map_err(|error| {
            crate::Error::new(
                Code::StateUnwritable,
                format!("could not create journal {}: {error}", dir.display()),
            )
        })?;
        // `write_json_atomic` flushes the file's own contents before the
        // rename makes it visible, so the plan is on the platter before any
        // target file is touched.
        write_json_atomic(&dir.join(PLAN_FILE), &record)?;
        Ok(Self {
            dir,
            record,
            applied: Vec::new(),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn record(&self) -> &JournalRecord {
        &self.record
    }

    /// Record that a step has fully landed - written, fsynced and verified.
    ///
    /// Appended and flushed one line at a time rather than buffered, because a
    /// buffered progress log is a progress log that does not exist when it is
    /// needed. The cost is one fsync per file, against a copy that already
    /// cost one.
    pub fn note_applied(&mut self, index: usize) -> Result<()> {
        let line = format!("{{\"i\":{index}}}\n");
        let path = self.dir.join(PROGRESS_FILE);
        let mut handle = File::options()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(unwritable("could not open progress log", &path))?;
        handle
            .write_all(line.as_bytes())
            .map_err(unwritable("could not append to progress log", &path))?;
        handle
            .sync_all()
            .map_err(unwritable("could not flush progress log", &path))?;
        self.applied.push(index);
        Ok(())
    }

    /// Mark the install complete. Written last, after every step has been
    /// verified, which is what makes its absence meaningful.
    pub fn commit(&self) -> Result<()> {
        write_atomic(
            &self.dir.join(COMMIT_FILE),
            JOURNAL_VERSION.to_string().as_bytes(),
        )
    }

    /// Remove the journal directory. Never touches the backup store.
    pub fn remove(&self) -> Result<()> {
        remove_tree(&self.dir)
    }

    pub fn applied(&self) -> &[usize] {
        &self.applied
    }
}

/// A journal found on disk, with enough read back to decide about it.
#[derive(Debug, Clone)]
pub struct Survey {
    pub dir: PathBuf,
    pub state: JournalState,
    /// `None` when the plan is missing or unreadable, which the state records.
    pub record: Option<JournalRecord>,
    pub recovery: Recovery,
}

/// Read every journal under `journal_root` and decide about each.
///
/// A directory that cannot be inspected at all is reported as a journal whose
/// plan is unreadable, which quarantines it. Skipping it silently would mean a
/// half-changed game folder nobody ever hears about.
pub fn survey(journal_root: &Path) -> Result<Vec<Survey>> {
    let entries = match fs::read_dir(journal_root) {
        Ok(entries) => entries,
        // No journal directory at all is the ordinary case: nothing has ever
        // been installed, or every install cleaned up after itself.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return fail(
                Code::JournalCorrupt,
                format!(
                    "could not list journals in {}: {error}",
                    journal_root.display()
                ),
            )
        }
    };

    let mut found = Vec::new();
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        found.push(survey_one(&entry.path()));
    }
    // Oldest first, so a rollback of an older journal happens before a newer
    // one that may have touched the same file.
    found.sort_by(|left, right| left.state.id.cmp(&right.state.id));
    Ok(found)
}

fn survey_one(dir: &Path) -> Survey {
    let id = dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();

    let plan_path = dir.join(PLAN_FILE);
    let has_plan = plan_path.is_file();
    let record: Option<JournalRecord> = fs::read_to_string(&plan_path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        // A journal written by a newer build is deliberately treated as
        // unreadable rather than interpreted through this build's assumptions.
        .filter(|parsed: &JournalRecord| parsed.version <= JOURNAL_VERSION);

    let state = JournalState {
        id,
        has_plan,
        plan_readable: record.is_some(),
        committed: dir.join(COMMIT_FILE).is_file(),
        applied_steps: count_progress(&dir.join(PROGRESS_FILE)),
        total_steps: record.as_ref().map_or(0, |parsed| {
            parsed.steps.len().try_into().unwrap_or(i64::MAX)
        }),
    };

    Survey {
        dir: dir.to_path_buf(),
        recovery: decide_recovery(&state),
        state,
        record,
    }
}

/// Count the complete lines in the progress log.
///
/// A crash can leave the last line half-written, and a half-written line is
/// not a step that finished. Counting newlines rather than lines is what makes
/// that distinction: the trailing fragment has no newline and is not counted.
fn count_progress(path: &Path) -> i64 {
    let Ok(text) = fs::read_to_string(path) else {
        return 0;
    };
    text.lines()
        .filter(|line| line.ends_with('}') && line.contains("\"i\""))
        .count()
        .try_into()
        .unwrap_or(i64::MAX)
}

/// What a recovery run actually did about one journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryOutcome {
    pub id: String,
    pub decision: RecoveryDecision,
    pub reason: String,
    /// Files put back where they were.
    pub restored: Vec<String>,
    /// Files we had created and have now removed.
    pub removed: Vec<String>,
    /// Anything that could not be undone. The journal is kept when this is
    /// non-empty, so a later run - or a person - can still see it.
    pub failures: Vec<String>,
}

/// Act on every journal found. Safe to run at every launch, and safe to run
/// twice: each individual undo is idempotent.
pub fn recover_all(journal_root: &Path) -> Result<Vec<RecoveryOutcome>> {
    let mut outcomes = Vec::new();
    for found in survey(journal_root)? {
        outcomes.push(recover_one(&found));
    }
    Ok(outcomes)
}

/// Act on one journal by path.
///
/// Used by an install that has just failed, to undo its own work immediately
/// rather than leaving it for the next launch. It goes through exactly the
/// same survey-and-decide path as recovery after a crash, so the immediate
/// rollback and the one that happens days later cannot drift apart.
pub fn recover_dir(dir: &Path) -> RecoveryOutcome {
    recover_one(&survey_one(dir))
}

fn recover_one(found: &Survey) -> RecoveryOutcome {
    let mut outcome = RecoveryOutcome {
        id: found.state.id.clone(),
        decision: found.recovery.decision,
        reason: format!("{:?}", found.recovery.reason),
        restored: Vec::new(),
        removed: Vec::new(),
        failures: Vec::new(),
    };

    match found.recovery.decision {
        // Nothing was changed, so there is nothing to undo. The backup
        // directory of an inert journal holds no originals worth keeping.
        RecoveryDecision::Discard => {
            if let Err(error) = remove_tree(&found.dir) {
                outcome.failures.push(error.to_string());
            }
        }
        // Committed: the bookkeeping goes, the backups stay. They belong to
        // the manifest now.
        RecoveryDecision::FinishCleanup => {
            if let Err(error) = remove_tree(&found.dir) {
                outcome.failures.push(error.to_string());
            }
        }
        // Kept exactly as found, untouched, for diagnosis.
        RecoveryDecision::Quarantine => {}
        RecoveryDecision::RollBack => roll_back(found, &mut outcome),
    }
    outcome
}

/// Undo the applied steps in reverse order.
///
/// Reverse matters: an earlier step may have created the directory a later one
/// wrote into, and undoing in order would try to remove it while it was still
/// occupied.
///
/// Every step is attempted even if an earlier one fails, because a failure to
/// restore one file is no reason to leave the other three swapped. What fails
/// is collected and the journal is kept.
fn roll_back(found: &Survey, outcome: &mut RecoveryOutcome) {
    let Some(record) = found.record.as_ref() else {
        outcome
            .failures
            .push("no readable plan to roll back".to_owned());
        return;
    };

    // The progress log says how many steps landed; the plan says what they
    // were. Anything past that count was never applied, so undoing it would
    // be inventing work - and in the `Create` case would mean deleting a file
    // that was already there before we started.
    let applied = usize::try_from(found.state.applied_steps).unwrap_or(0);
    for step in record.steps.iter().take(applied).rev() {
        let target = record.game_dir.join(step.rel.replace('\\', "/"));
        match step.action {
            StepAction::Replace => match restore(step, &target) {
                Ok(()) => outcome.restored.push(step.rel.clone()),
                Err(error) => outcome.failures.push(error.to_string()),
            },
            StepAction::Create => match remove_file_if_present(&target) {
                Ok(()) => outcome.removed.push(step.rel.clone()),
                Err(error) => outcome.failures.push(error.to_string()),
            },
            // A skip is never journalled, so reaching this means the record
            // disagrees with how it was written. Left alone and reported.
            StepAction::Skip => outcome
                .failures
                .push(format!("step {} is a skip in the journal", step.index)),
        }
    }

    if outcome.failures.is_empty() {
        // The originals are back where they belong, so the copies are spare.
        for step in record.steps.iter().take(applied) {
            if let Some(backup) = step.backup.as_ref() {
                let _ = fs::remove_file(backup);
            }
        }
        if let Err(error) = remove_tree(&found.dir) {
            outcome.failures.push(error.to_string());
        }
    }
}

/// Put one backup back, verifying it first.
fn restore(step: &JournalStep, target: &Path) -> Result<()> {
    let Some(backup) = step.backup.as_ref() else {
        return fail(
            Code::JournalCorrupt,
            format!("step {} replaced a file but kept no backup", step.index),
        );
    };
    if !backup.is_file() {
        // Already restored and cleaned up by an earlier recovery run, or the
        // file was never taken. Either way there is nothing to put back, and
        // saying so is better than failing a second run that has no work.
        if target.is_file() {
            return Ok(());
        }
        return fail(
            Code::JournalCorrupt,
            format!("backup for {} is missing: {}", step.rel, backup.display()),
        );
    }

    let data = fs::read(backup).map_err(|error| {
        crate::Error::new(
            Code::JournalCorrupt,
            format!("could not read backup {}: {error}", backup.display()),
        )
    })?;
    if let Some(expected) = step.replaced_sha256.as_ref() {
        // Writing a corrupted backup over the file it was meant to protect
        // would turn a recoverable interruption into a broken game.
        let actual = crate::hash::hash_bytes(&data);
        crate::hash::verify(backup, &actual, expected)?;
    }
    write_atomic(target, &data)
}

fn remove_file_if_present(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        // Already gone: an earlier recovery run got there, or the write never
        // landed. Idempotent by design, so this is success.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => fail(
            Code::StateUnwritable,
            format!("could not remove {}: {error}", path.display()),
        ),
    }
}

fn remove_tree(dir: &Path) -> Result<()> {
    match fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => fail(
            Code::StateUnwritable,
            format!("could not remove {}: {error}", dir.display()),
        ),
    }
}

fn unwritable<'a>(
    what: &'static str,
    path: &'a Path,
) -> impl Fn(std::io::Error) -> crate::Error + 'a {
    move |error| {
        crate::Error::new(
            Code::StateUnwritable,
            format!("{what} {}: {error}", path.display()),
        )
    }
}

/// A sortable, unique journal name: `<utc timestamp>-<counter>`.
///
/// Sortable because recovery must undo an older journal before a newer one
/// that may have touched the same file, and lexical order over a fixed-width
/// timestamp is chronological order.
pub fn new_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or(0);
    format!(
        "{millis:015}-{:04x}",
        COUNTER.fetch_add(1, Ordering::Relaxed) & 0xffff
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install::recover::RecoveryReason;

    fn record(dir: &Path, steps: Vec<JournalStep>) -> JournalRecord {
        JournalRecord {
            version: JOURNAL_VERSION,
            id: new_id(),
            game_dir: dir.to_path_buf(),
            route: Route::NativeDll,
            created_at: 0,
            steps,
        }
    }

    fn replace_step(index: usize, rel: &str, backup: &Path, replaced: &str) -> JournalStep {
        JournalStep {
            index,
            rel: rel.to_owned(),
            action: StepAction::Replace,
            expected_sha256: "new".to_owned(),
            expected_size: 3,
            backup: Some(backup.to_path_buf()),
            replaced_sha256: Some(replaced.to_owned()),
        }
    }

    #[test]
    fn a_journal_is_readable_the_moment_it_begins() {
        let root = tempfile::tempdir().expect("tempdir");
        let journals = root.path().join("journal");
        let game = root.path().join("game");

        let handle = Journal::begin(&journals, record(&game, vec![])).expect("begin");
        assert!(handle.dir().join(PLAN_FILE).is_file());

        let found = survey(&journals).expect("survey");
        assert_eq!(found.len(), 1);
        assert!(found[0].state.has_plan);
        assert!(found[0].state.plan_readable);
        assert!(!found[0].state.committed);
    }

    #[test]
    fn no_journal_directory_is_not_an_error() {
        let root = tempfile::tempdir().expect("tempdir");
        assert!(survey(&root.path().join("never-created"))
            .expect("survey")
            .is_empty());
    }

    #[test]
    fn a_half_written_progress_line_is_not_a_finished_step() {
        let root = tempfile::tempdir().expect("tempdir");
        let journals = root.path().join("journal");
        let game = root.path().join("game");
        let mut handle = Journal::begin(
            &journals,
            record(
                &game,
                vec![replace_step(0, "a.dll", &game.join("bak"), "old")],
            ),
        )
        .expect("begin");
        handle.note_applied(0).expect("note");

        // Simulate a crash mid-append: a line with no newline behind it.
        let progress = handle.dir().join(PROGRESS_FILE);
        let mut file = File::options().append(true).open(&progress).expect("open");
        file.write_all(b"{\"i\":1").expect("write");
        drop(file);

        // One complete line, not two.
        assert_eq!(count_progress(&progress), 1);
    }

    #[test]
    fn a_rollback_restores_the_original_bytes() {
        let root = tempfile::tempdir().expect("tempdir");
        let journals = root.path().join("journal");
        let game = root.path().join("game");
        let backups = root.path().join("backups");
        fs::create_dir_all(&game).expect("game dir");
        fs::create_dir_all(&backups).expect("backup dir");

        // The original, and a copy of it taken aside.
        let target = game.join("nvngx_dlss.dll");
        fs::write(&target, b"original").expect("write original");
        let backup = backups.join("0000.bin");
        fs::copy(&target, &backup).expect("copy aside");
        let original_hash = crate::hash::hash_bytes(b"original");

        // The install replaced it and then stopped without committing.
        fs::write(&target, b"replacement").expect("write replacement");
        let mut handle = Journal::begin(
            &journals,
            record(
                &game,
                vec![replace_step(0, "nvngx_dlss.dll", &backup, &original_hash)],
            ),
        )
        .expect("begin");
        handle.note_applied(0).expect("note");

        let outcomes = recover_all(&journals).expect("recover");
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].decision, RecoveryDecision::RollBack);
        assert!(outcomes[0].failures.is_empty(), "{:?}", outcomes[0]);
        assert_eq!(fs::read(&target).expect("read back"), b"original");
        // The journal and the now-spare backup are gone.
        assert!(!handle.dir().exists());
        assert!(!backup.exists());
    }

    #[test]
    fn a_rollback_removes_a_file_the_install_created() {
        let root = tempfile::tempdir().expect("tempdir");
        let journals = root.path().join("journal");
        let game = root.path().join("game");
        fs::create_dir_all(&game).expect("game dir");
        let target = game.join("sl.dlss_g.dll");
        fs::write(&target, b"brand new").expect("write");

        let mut handle = Journal::begin(
            &journals,
            record(
                &game,
                vec![JournalStep {
                    index: 0,
                    rel: "sl.dlss_g.dll".to_owned(),
                    action: StepAction::Create,
                    expected_sha256: "x".to_owned(),
                    expected_size: 9,
                    backup: None,
                    replaced_sha256: None,
                }],
            ),
        )
        .expect("begin");
        handle.note_applied(0).expect("note");

        let outcomes = recover_all(&journals).expect("recover");
        assert_eq!(outcomes[0].removed, vec!["sl.dlss_g.dll"]);
        assert!(!target.exists());
    }

    #[test]
    fn a_step_that_never_applied_is_not_undone() {
        // The bug this guards: rolling back the whole plan rather than the
        // part that ran would delete a file that was there before we started.
        let root = tempfile::tempdir().expect("tempdir");
        let journals = root.path().join("journal");
        let game = root.path().join("game");
        fs::create_dir_all(&game).expect("game dir");

        let untouched = game.join("second.dll");
        fs::write(&untouched, b"was here all along").expect("write");
        let created = game.join("first.dll");
        fs::write(&created, b"ours").expect("write");

        let make = |index: usize, rel: &str| JournalStep {
            index,
            rel: rel.to_owned(),
            action: StepAction::Create,
            expected_sha256: "x".to_owned(),
            expected_size: 4,
            backup: None,
            replaced_sha256: None,
        };
        let mut handle = Journal::begin(
            &journals,
            record(&game, vec![make(0, "first.dll"), make(1, "second.dll")]),
        )
        .expect("begin");
        // Only the first step landed.
        handle.note_applied(0).expect("note");

        recover_all(&journals).expect("recover");
        assert!(!created.exists(), "our file should be gone");
        assert!(untouched.is_file(), "a file we never wrote must survive");
    }

    #[test]
    fn a_corrupted_backup_is_refused_rather_than_written_back() {
        let root = tempfile::tempdir().expect("tempdir");
        let journals = root.path().join("journal");
        let game = root.path().join("game");
        let backups = root.path().join("backups");
        fs::create_dir_all(&game).expect("game dir");
        fs::create_dir_all(&backups).expect("backup dir");

        let target = game.join("nvngx_dlss.dll");
        fs::write(&target, b"replacement").expect("write");
        let backup = backups.join("0000.bin");
        fs::write(&backup, b"corrupted on disk").expect("write backup");

        let mut handle = Journal::begin(
            &journals,
            record(
                &game,
                vec![replace_step(
                    0,
                    "nvngx_dlss.dll",
                    &backup,
                    &crate::hash::hash_bytes(b"original"),
                )],
            ),
        )
        .expect("begin");
        handle.note_applied(0).expect("note");

        let outcomes = recover_all(&journals).expect("recover");
        assert_eq!(outcomes[0].failures.len(), 1);
        assert!(outcomes[0].failures[0].contains("verifyFailed"));
        // Nothing was written, and the journal is kept so it can be looked at.
        assert_eq!(fs::read(&target).expect("read"), b"replacement");
        assert!(handle.dir().exists());
    }

    #[test]
    fn a_committed_journal_is_removed_but_its_backups_are_kept() {
        let root = tempfile::tempdir().expect("tempdir");
        let journals = root.path().join("journal");
        let game = root.path().join("game");
        let backups = root.path().join("backups");
        fs::create_dir_all(&backups).expect("backup dir");
        let backup = backups.join("0000.bin");
        fs::write(&backup, b"the original").expect("write backup");

        let mut handle = Journal::begin(
            &journals,
            record(
                &game,
                vec![replace_step(
                    0,
                    "a.dll",
                    &backup,
                    &crate::hash::hash_bytes(b"the original"),
                )],
            ),
        )
        .expect("begin");
        handle.note_applied(0).expect("note");
        handle.commit().expect("commit");

        let outcomes = recover_all(&journals).expect("recover");
        assert_eq!(outcomes[0].decision, RecoveryDecision::FinishCleanup);
        assert!(!handle.dir().exists(), "the journal is bookkeeping");
        assert!(backup.is_file(), "the backup belongs to the manifest now");
    }

    #[test]
    fn a_journal_from_a_newer_build_is_quarantined_not_reinterpreted() {
        let root = tempfile::tempdir().expect("tempdir");
        let journals = root.path().join("journal");
        let dir = journals.join("999999999999999-0000");
        fs::create_dir_all(&dir).expect("dir");
        let mut future = record(&root.path().join("game"), vec![]);
        future.version = JOURNAL_VERSION + 1;
        write_json_atomic(&dir.join(PLAN_FILE), &future).expect("write");

        let found = survey(&journals).expect("survey");
        assert_eq!(found[0].recovery.decision, RecoveryDecision::Quarantine);
        assert_eq!(found[0].recovery.reason, RecoveryReason::PlanUnreadable);
        // Quarantine means untouched.
        recover_all(&journals).expect("recover");
        assert!(dir.join(PLAN_FILE).is_file());
    }

    #[test]
    fn recovery_can_be_run_twice() {
        // Recovery itself can be interrupted, so a second pass must not fail
        // on work the first pass already did.
        let root = tempfile::tempdir().expect("tempdir");
        let journals = root.path().join("journal");
        let game = root.path().join("game");
        fs::create_dir_all(&game).expect("game dir");
        fs::write(game.join("first.dll"), b"ours").expect("write");

        let mut handle = Journal::begin(
            &journals,
            record(
                &game,
                vec![JournalStep {
                    index: 0,
                    rel: "first.dll".to_owned(),
                    action: StepAction::Create,
                    expected_sha256: "x".to_owned(),
                    expected_size: 4,
                    backup: None,
                    replaced_sha256: None,
                }],
            ),
        )
        .expect("begin");
        handle.note_applied(0).expect("note");

        let first = recover_all(&journals).expect("first pass");
        assert!(first[0].failures.is_empty());
        let second = recover_all(&journals).expect("second pass");
        assert!(second.is_empty(), "nothing should be left to recover");
    }

    #[test]
    fn journals_are_surveyed_oldest_first() {
        let root = tempfile::tempdir().expect("tempdir");
        let journals = root.path().join("journal");
        let game = root.path().join("game");
        let mut ids = Vec::new();
        for _ in 0..3 {
            let handle = Journal::begin(&journals, record(&game, vec![])).expect("begin");
            ids.push(handle.record().id.clone());
        }
        let found = survey(&journals).expect("survey");
        let seen: Vec<String> = found.iter().map(|one| one.state.id.clone()).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(seen, sorted);
    }
}
