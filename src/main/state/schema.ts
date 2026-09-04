/** Shape of persisted settings, plus the migration ladder that produces it. */

export interface ScanRecord {
  ok: boolean;
  api: string | null;
  apiLabel: string | null;
  bitness: 32 | 64 | null;
  exe: string | null;
  dlssVersion: string | null;
  routes: string[];
  reason: string | null;
  scannedAt: number;
  /** Detection-logic generation. A bump invalidates every cached verdict. */
  rules: number;
}

export interface ArtRecord {
  appid: number | null;
  cover: string | null;
  hero: string | null;
  /** Art-selection generation, same idea as ScanRecord.rules. */
  rules: number;
  fetchedAt: number;
  /** Remembering a miss is what stops us asking Steam again on every launch. */
  miss?: boolean;
}

export interface Settings {
  schema: number;
  theme: 'light' | 'dark' | 'system';
  lang: string;
  groupGamesByStore: boolean;
  autoScanDrives: boolean;
  folders: string[];
  excludedRoots: string[];
  manual: string[];
  hidden: string[];
  posters: Record<string, string>;
  scans: Record<string, ScanRecord>;
  art: Record<string, ArtRecord>;
  recents: { dir: string; at: number }[];
  addons: { path: string; name: string | null; notes: string[]; enabled: boolean }[];
}

export const SCHEMA_VERSION = 2;

export function defaults(): Settings {
  return {
    schema: SCHEMA_VERSION,
    theme: 'system',
    lang: 'en',
    groupGamesByStore: true,
    // Sweeping every drive without being asked is a surprise, not a feature.
    autoScanDrives: false,
    folders: [],
    excludedRoots: [],
    manual: [],
    hidden: [],
    posters: {},
    scans: {},
    art: {},
    recents: [],
    addons: []
  };
}

type Migration = (input: Record<string, unknown>) => Record<string, unknown>;

/**
 * Keyed by the version being migrated *from*. Each step is total: it must cope
 * with fields that are missing or the wrong type, because the file it is
 * handed was written by an older build or edited by hand.
 */
export const MIGRATIONS: Record<number, Migration> = {
  /**
   * v1 is the upstream `library.json` layout, which spread add-ons over three
   * fields that had drifted apart over releases:
   *
   *   `addon`      - a single enabled path, from the oldest builds
   *   `addons`     - the enabled paths, as bare strings
   *   `addonFiles` - the catalogue of hand-added builds, with names and notes
   *
   * They collapse into one list of catalogue entries carrying an `enabled`
   * flag. Enablement is the union of the first two; metadata comes from the
   * third. A build that was enabled but never catalogued still has to appear,
   * or migrating would quietly turn off something the user had switched on.
   */
  1: (input) => {
    const out = { ...input };

    const pathOf = (entry: unknown): string | null => {
      if (typeof entry === 'string') return entry;
      if (typeof entry === 'object' && entry !== null) {
        const candidate = (entry as { path?: unknown }).path;
        if (typeof candidate === 'string') return candidate;
      }
      return null;
    };

    const enabled = new Set<string>();
    for (const entry of Array.isArray(input['addons']) ? (input['addons'] as unknown[]) : []) {
      const file = pathOf(entry);
      if (file) enabled.add(file.toLowerCase());
    }
    if (typeof input['addon'] === 'string') enabled.add(input['addon'].toLowerCase());

    const catalogue = new Map<string, { path: string; name: string | null; notes: string[] }>();
    const remember = (file: string, name: unknown, notes: unknown): void => {
      const key = file.toLowerCase();
      const existing = catalogue.get(key);
      catalogue.set(key, {
        path: existing?.path ?? file,
        name: typeof name === 'string' && name.trim() !== '' ? name.trim() : (existing?.name ?? null),
        notes: Array.isArray(notes)
          ? notes.filter((note): note is string => typeof note === 'string')
          : (existing?.notes ?? [])
      });
    };

    for (const entry of Array.isArray(input['addonFiles']) ? (input['addonFiles'] as unknown[]) : []) {
      const file = pathOf(entry);
      if (!file) continue;
      const row = typeof entry === 'object' && entry !== null ? (entry as Record<string, unknown>) : {};
      remember(file, row['name'], row['notes']);
    }
    // Anything enabled but absent from the catalogue is added bare, so the
    // switched-on state survives even when its description does not.
    for (const entry of Array.isArray(input['addons']) ? (input['addons'] as unknown[]) : []) {
      const file = pathOf(entry);
      if (file && !catalogue.has(file.toLowerCase())) remember(file, null, null);
    }
    if (typeof input['addon'] === 'string' && !catalogue.has(input['addon'].toLowerCase())) {
      remember(input['addon'], null, null);
    }

    out['addons'] = [...catalogue.entries()].map(([key, row]) => ({
      path: row.path,
      name: row.name,
      notes: row.notes,
      enabled: enabled.has(key)
    }));
    delete out['addon'];
    delete out['addonFiles'];
    out['schema'] = 2;
    return out;
  }
};
