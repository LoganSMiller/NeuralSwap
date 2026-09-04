//! Deciding what to do about a journal left behind by an interrupted install.
//!
//! NTFS gives no way to replace several files as one transaction, so an install
//! that touches four DLLs has three moments where a power cut leaves the folder
//! half-changed. The answer is not to pretend otherwise but to write down the
//! intent first and make the recovery deterministic.
//!
//! This function is the whole decision, separated out and kept pure: given
//! what survived on disk, what should happen? Doing it as data means every
//! branch is testable without staging a crash, including the ones that are
//! otherwise almost impossible to reach on purpose.
//!
//! The default for a half-applied install is to roll back, not to press on.
//! Finishing needs the source package to still be there and still be the same
//! package, and neither can be assumed after an unknown amount of time and an
//! unclean shutdown. Rolling back needs only the backups, which are right
//! there beside the journal, so it is the option that is always available -
//! and it lands the user somewhere they recognise.

use serde::{Deserialize, Serialize};

/// What a journal directory looks like after an unclean stop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JournalState {
    pub id: String,
    /// The intent file exists. Without it there is nothing to act on.
    pub has_plan: bool,
    /// The intent file parsed. A journal we cannot read is not one we can undo.
    pub plan_readable: bool,
    /// The commit marker, written only once every step has landed and verified.
    pub committed: bool,
    /// Steps recorded as applied, from the append-only progress log.
    pub applied_steps: i64,
    /// Steps the plan called for.
    pub total_steps: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RecoveryDecision {
    /// Journal is inert; nothing was changed. Remove it.
    Discard,
    /// Restore the backups, in reverse order, then remove it.
    RollBack,
    /// Everything landed. Remove the journal - but not the backups, which the
    /// install manifest points at so the game's original files can be put back
    /// later. Cleanup here means the bookkeeping, not the safety net.
    FinishCleanup,
    /// We cannot reason about it. Keep it, untouched, and tell the user.
    Quarantine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RecoveryReason {
    NoPlan,
    PlanUnreadable,
    Committed,
    NothingApplied,
    PartiallyApplied,
    ProgressExceedsPlan,
    NegativeCounts,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Recovery {
    pub decision: RecoveryDecision,
    pub reason: RecoveryReason,
}

/// The counts are `i64` rather than `usize` on purpose: they are read back
/// from files a crash may have truncated mid-line, and the sanity check below
/// has to be able to see a negative rather than have it wrap to enormous.
pub fn decide_recovery(state: &JournalState) -> Recovery {
    let verdict =
        |decision: RecoveryDecision, reason: RecoveryReason| Recovery { decision, reason };

    if state.applied_steps < 0 || state.total_steps < 0 {
        return verdict(RecoveryDecision::Quarantine, RecoveryReason::NegativeCounts);
    }
    // A directory with no plan can only be one that was being created when the
    // process stopped - the plan is fsynced before any target file is opened.
    if !state.has_plan {
        return verdict(RecoveryDecision::Discard, RecoveryReason::NoPlan);
    }
    if !state.plan_readable {
        return verdict(RecoveryDecision::Quarantine, RecoveryReason::PlanUnreadable);
    }
    // More progress than plan means one of the two files is lying. Guessing
    // which would mean guessing what to restore.
    if state.applied_steps > state.total_steps {
        return verdict(
            RecoveryDecision::Quarantine,
            RecoveryReason::ProgressExceedsPlan,
        );
    }
    // Committed is checked after the count sanity checks and before the applied
    // count, because a committed journal is finished regardless of what the
    // progress log says about how it got there.
    if state.committed {
        return verdict(RecoveryDecision::FinishCleanup, RecoveryReason::Committed);
    }
    if state.applied_steps == 0 {
        return verdict(RecoveryDecision::Discard, RecoveryReason::NothingApplied);
    }
    verdict(RecoveryDecision::RollBack, RecoveryReason::PartiallyApplied)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> JournalState {
        JournalState {
            id: "j1".to_owned(),
            has_plan: true,
            plan_readable: true,
            committed: false,
            applied_steps: 0,
            total_steps: 4,
        }
    }

    #[test]
    fn a_journal_with_no_plan_was_still_being_created() {
        let mut subject = state();
        subject.has_plan = false;
        assert_eq!(
            decide_recovery(&subject),
            Recovery {
                decision: RecoveryDecision::Discard,
                reason: RecoveryReason::NoPlan
            }
        );
    }

    #[test]
    fn an_unreadable_plan_is_kept_not_guessed_at() {
        let mut subject = state();
        subject.plan_readable = false;
        subject.applied_steps = 2;
        assert_eq!(
            decide_recovery(&subject).decision,
            RecoveryDecision::Quarantine
        );
    }

    #[test]
    fn a_half_applied_install_rolls_back() {
        let mut subject = state();
        subject.applied_steps = 2;
        assert_eq!(
            decide_recovery(&subject),
            Recovery {
                decision: RecoveryDecision::RollBack,
                reason: RecoveryReason::PartiallyApplied
            }
        );
    }

    #[test]
    fn every_step_applied_but_uncommitted_still_rolls_back() {
        // The marker is written after the last verification. Without it we
        // cannot claim the last file is intact.
        let mut subject = state();
        subject.applied_steps = 4;
        assert_eq!(
            decide_recovery(&subject).decision,
            RecoveryDecision::RollBack
        );
    }

    #[test]
    fn a_committed_journal_only_needs_clearing() {
        let mut subject = state();
        subject.committed = true;
        subject.applied_steps = 3;
        assert_eq!(
            decide_recovery(&subject),
            Recovery {
                decision: RecoveryDecision::FinishCleanup,
                reason: RecoveryReason::Committed
            }
        );
    }

    #[test]
    fn nonsensical_counts_are_quarantined_first() {
        let mut subject = state();
        subject.applied_steps = -1;
        assert_eq!(
            decide_recovery(&subject),
            Recovery {
                decision: RecoveryDecision::Quarantine,
                reason: RecoveryReason::NegativeCounts
            }
        );

        let mut committed = state();
        committed.total_steps = -4;
        committed.committed = true;
        assert_eq!(
            decide_recovery(&committed).reason,
            RecoveryReason::NegativeCounts
        );
    }

    #[test]
    fn more_progress_than_plan_is_quarantined() {
        let mut subject = state();
        subject.applied_steps = 5;
        assert_eq!(
            decide_recovery(&subject).reason,
            RecoveryReason::ProgressExceedsPlan
        );
    }
}
