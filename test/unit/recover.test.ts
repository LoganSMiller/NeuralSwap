import assert from 'node:assert/strict';
import { test } from 'node:test';
import { decideRecovery, type JournalState } from '../../src/core/install/recover.ts';

const state = (over: Partial<JournalState> = {}): JournalState => ({
  id: 'j1',
  hasPlan: true,
  planReadable: true,
  committed: false,
  appliedSteps: 0,
  totalSteps: 4,
  ...over,
});

test('a journal with no plan was still being created', () => {
  // The plan is fsynced before any target file is opened, so its absence
  // proves nothing was touched.
  assert.deepEqual(decideRecovery(state({ hasPlan: false })), {
    decision: 'discard',
    reason: 'noPlan',
  });
});

test('an unreadable plan is kept, not guessed at', () => {
  // Without the plan we do not know which backup belongs to which target.
  // Deleting it would throw away the only record of a half-changed folder.
  assert.deepEqual(decideRecovery(state({ planReadable: false })), {
    decision: 'quarantine',
    reason: 'planUnreadable',
  });
});

test('a committed journal only needs its backups cleared', () => {
  assert.deepEqual(decideRecovery(state({ committed: true, appliedSteps: 4 })), {
    decision: 'finishCleanup',
    reason: 'committed',
  });
});

test('a plan that never applied a step is inert', () => {
  assert.deepEqual(decideRecovery(state({ appliedSteps: 0 })), {
    decision: 'discard',
    reason: 'nothingApplied',
  });
});

test('a half-applied install rolls back rather than pressing on', () => {
  // Rolling forward would need the source package to still exist and still be
  // the same package. Rolling back needs only the backups, which are here.
  assert.deepEqual(decideRecovery(state({ appliedSteps: 2 })), {
    decision: 'rollBack',
    reason: 'partiallyApplied',
  });
});

test('every step applied but no commit marker still rolls back', () => {
  // The marker is written after the last verification. Without it we cannot
  // claim the last file is intact, so the folder is not known-good.
  assert.deepEqual(decideRecovery(state({ appliedSteps: 4, totalSteps: 4 })), {
    decision: 'rollBack',
    reason: 'partiallyApplied',
  });
});

test('more progress than plan means one of the files is lying', () => {
  assert.deepEqual(decideRecovery(state({ appliedSteps: 5, totalSteps: 4 })), {
    decision: 'quarantine',
    reason: 'progressExceedsPlan',
  });
});

test('nonsensical counts are quarantined before anything else is believed', () => {
  // A crash can truncate the progress log mid-line; the parser must not be
  // able to talk us into restoring a negative number of files.
  assert.deepEqual(decideRecovery(state({ appliedSteps: -1 })), {
    decision: 'quarantine',
    reason: 'negativeCounts',
  });
  assert.deepEqual(decideRecovery(state({ totalSteps: -4, committed: true })), {
    decision: 'quarantine',
    reason: 'negativeCounts',
  });
});

test('a committed journal is finished even if progress looks short', () => {
  // The marker is the authority on completion: it is written last, and only
  // after every step verified. A progress log missing its final fsync is a
  // known and harmless outcome.
  assert.deepEqual(decideRecovery(state({ committed: true, appliedSteps: 3, totalSteps: 4 })), {
    decision: 'finishCleanup',
    reason: 'committed',
  });
});

test('an empty plan that committed is cleanup, not a rollback', () => {
  assert.deepEqual(decideRecovery(state({ committed: true, totalSteps: 0, appliedSteps: 0 })), {
    decision: 'finishCleanup',
    reason: 'committed',
  });
});
