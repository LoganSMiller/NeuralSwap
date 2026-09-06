/**
 * The human name for each preflight check.
 *
 * Lives in `shared` rather than beside the view because it is half of a
 * cross-language contract: the other half is `CheckName` in
 * `install/preflight.rs`, and `spec/checks.json` holds both sides to the same
 * list. A check added in Rust with no entry here is a build failure rather
 * than a user staring at `driverOverride`.
 *
 * These are labels, not explanations. The sentence a user needs is the check's
 * own `detail`, which is written where the check runs and knows the specifics.
 * Keep the order the order the checks run in.
 */
export const CHECK_LABELS = {
  gameDirectory: 'Game folder',
  storeProtected: 'Folder permissions',
  pathSafety: 'Target paths',
  writable: 'Writable',
  filesInUse: 'Files in use',
  diskSpace: 'Disk space',
  sourceFiles: 'Package contents',
  graphicsCard: 'Graphics card',
  otherTools: 'Other tools',
  driverOverride: 'Driver settings',
  antiCheat: 'Anti-cheat',
  remixMod: 'RTX Remix mod',
} as const satisfies Record<string, string>;

export type CheckName = keyof typeof CHECK_LABELS;

/** The wire names, in order, for the spec vector. */
export const CHECK_NAMES = Object.keys(CHECK_LABELS) as CheckName[];
