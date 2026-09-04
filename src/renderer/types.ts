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
