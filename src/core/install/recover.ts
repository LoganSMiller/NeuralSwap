/**
 * Deciding what to do about a journal left behind by an interrupted install.
 *
 * NTFS gives no way to replace several files as one transaction, so an install
 * that touches four DLLs has three moments where a power cut leaves the folder
 * half-changed. The answer is not to pretend otherwise but to write down the
 * intent first and make the recovery deterministic.
 *
 * This function is the whole decision, separated out and kept pure: given what
 * survived on disk, what should happen? Doing it as data means every branch is
 * testable without staging a crash, including the ones that are otherwise
 * almost impossible to reach on purpose.
 *
 * The default for a half-applied install is to roll back, not to press on.
 * Finishing needs the source package to still be there and still be the same
 * package, and neither can be assumed after an unknown amount of time and an
 * unclean shutdown. Rolling back needs only the backups, which are right there
 * beside the journal, so it is the option that is always available - and it
 * lands the user somewhere they recognise.
 */

/** What a journal directory looks like after an unclean stop. */
export type JournalState = {
  readonly id: string;
  /** The intent file exists. Without it there is nothing to act on. */
  readonly hasPlan: boolean;
  /** The intent file parsed. A journal we cannot read is not one we can undo. */
  readonly planReadable: boolean;
  /** The commit marker, written only once every step has landed and verified. */
  readonly committed: boolean;
  /** Steps recorded as applied, from the append-only progress log. */
  readonly appliedSteps: number;
  /** Steps the plan called for. */
  readonly totalSteps: number;
};

export type RecoveryDecision =
  /** Journal is inert; nothing was changed. Remove it. */
  | 'discard'
  /** Restore the backups, in reverse order, then remove it. */
  | 'rollBack'
  /**
   * Everything landed. Remove the journal - but not the backups, which the
   * install manifest points at so the game's original files can be put back
   * later. Cleanup here means the bookkeeping, not the safety net.
   */
  | 'finishCleanup'
  /** We cannot reason about it. Keep it, untouched, and tell the user. */
  | 'quarantine';

export type RecoveryReason =
  | 'noPlan'
  | 'planUnreadable'
  | 'committed'
  | 'nothingApplied'
  | 'partiallyApplied'
  | 'progressExceedsPlan'
  | 'negativeCounts';

export type Recovery = {
  readonly decision: RecoveryDecision;
  readonly reason: RecoveryReason;
};

export function decideRecovery(state: JournalState): Recovery {
  // Counts come from a file that a crash may have truncated mid-line, so they
  // are not trusted to be sane before they are compared.
  if (state.appliedSteps < 0 || state.totalSteps < 0) {
    return { decision: 'quarantine', reason: 'negativeCounts' };
  }
  // A directory with no plan can only be one that was being created when the
  // process stopped - the plan is fsynced before any target file is opened.
  if (!state.hasPlan) {
    return { decision: 'discard', reason: 'noPlan' };
  }
  if (!state.planReadable) {
    return { decision: 'quarantine', reason: 'planUnreadable' };
  }
  // More progress than plan means one of the two files is lying. Guessing
  // which would mean guessing what to restore.
  if (state.appliedSteps > state.totalSteps) {
    return { decision: 'quarantine', reason: 'progressExceedsPlan' };
  }
  // Committed is checked after the count sanity checks and before the applied
  // count, because a committed journal is finished regardless of what the
  // progress log says about how it got there.
  if (state.committed) {
    return { decision: 'finishCleanup', reason: 'committed' };
  }
  if (state.appliedSteps === 0) {
    return { decision: 'discard', reason: 'nothingApplied' };
  }
  return { decision: 'rollBack', reason: 'partiallyApplied' };
}
