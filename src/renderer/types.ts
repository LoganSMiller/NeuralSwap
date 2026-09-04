/** Shapes the Rust side sends. Kept in one place so views agree on them. */

export type Theme = 'light' | 'dark' | 'system';

export type LoadStatus =
  | 'fresh'
  | 'loaded'
  | 'migrated'
  | 'recoveredFromBackup'
  | 'quarantined';

export interface Health {
  status: LoadStatus;
  quarantinedTo: string | null;
  writeError: { code: string; message: string } | null;
}

export interface BootInfo {
  version: string;
  theme: Theme;
  lang: string;
  groupGamesByStore: boolean;
  settingsHealth: Health;
}

export interface Verdict {
  api: string;
  label: string;
  fromMarker: boolean;
}

export interface Candidate {
  rel: string;
  bitness: number;
  api: Verdict | null;
  size: number;
  fileVersion: string | null;
  likelyHelper: boolean;
}

export interface RuntimeFile {
  rel: string;
  kind: string;
  version: string | null;
  provenance: string;
}

export interface ScanStats {
  entriesExamined: number;
  directoriesWalked: number;
  binariesParsed: number;
  cacheHits: number;
  walkMs: number;
  parseMs: number;
}

export interface FolderScan {
  dir: string;
  candidates: Candidate[];
  chosen: number | null;
  reason: string | null;
  runtimeFiles: RuntimeFile[];
  excluded: string[];
  stats: ScanStats;
}

export interface CacheInfo {
  entries: number;
  pruned: number;
}

export interface Settings {
  schema: number;
  theme: Theme;
  lang: string;
  groupGamesByStore: boolean;
  autoScanDrives: boolean;
  folders: string[];
  manual: string[];
  scans: Record<string, unknown>;
}

// ----------------------------------------------------------------- installing

export type StepAction = 'create' | 'replace' | 'skip';

export type StepReason =
  | 'newFile'
  | 'identical'
  | 'upgrade'
  | 'downgrade'
  | 'sameVersionDifferentBytes'
  | 'versionUnknown';

export interface Step {
  rel: string;
  action: StepAction;
  reason: StepReason;
  kind: string;
  fromVersion: string | null;
  toVersion: string | null;
  writeBytes: number;
  backupBytes: number;
  sha256: string;
}

export interface PlanWarning {
  code: string;
  rels: string[];
}

export interface Plan {
  route: string;
  installDir: string;
  steps: Step[];
  warnings: PlanWarning[];
  writeBytes: number;
  backupBytes: number;
  changes: number;
}

export type CheckOutcome = 'pass' | 'warn' | 'fail' | 'unknown';

export interface Check {
  name: string;
  outcome: CheckOutcome;
  detail: string;
  code: string | null;
}

export interface Preflight {
  checks: Check[];
  ok: boolean;
}

export interface PlanReply {
  plan: Plan;
  preflight: Preflight;
  busy: boolean;
}

/**
 * How far a failed install got. `nothingWritten` is a stronger guarantee than
 * `rolledBack`: nothing was ever touched, rather than touched and put back.
 */
export type Reached = 'nothingWritten' | 'rolledBack' | 'partiallyApplied';

/**
 * The Rust enums are internally tagged, so the variant's own fields sit
 * alongside the `outcome` discriminant rather than nested under it.
 */
export type InstallOutcome =
  | {
      outcome: 'installed';
      journalId: string;
      installed: string[];
      skipped: string[];
      bytesWritten: number;
    }
  | { outcome: 'refused'; checks: Check[]; ok: boolean }
  | {
      outcome: 'failed';
      code: string;
      message: string;
      reached: Reached;
      rollbackFailures: string[];
    };

export type FileStatus = 'intact' | 'changed' | 'missing' | 'unreadable';

export interface FileReport {
  rel: string;
  status: FileStatus;
  foundSha256: string | null;
  restorable: boolean;
}

export interface Integrity {
  files: FileReport[];
  intact: boolean;
}

export type RestoreAction =
  | 'restoredOriginal'
  | 'removedOurs'
  | 'leftAlone'
  | 'failed';

export interface RestoreFile {
  rel: string;
  action: RestoreAction;
  detail: string;
  code: string | null;
}

export type RestoreOutcome =
  | { outcome: 'nothingInstalled' }
  | { outcome: 'restored'; files: RestoreFile[]; complete: boolean };
