import fs from 'node:fs';
import { readFileOrNull, writeJsonAtomic } from '../../core/fsx/atomic.ts';
import { AppError, errnoOf, fail } from '../../shared/errors.ts';
import { MIGRATIONS, SCHEMA_VERSION, defaults, type Settings } from './schema.ts';
import { sanitize } from './sanitize.ts';

export type LoadStatus =
  /** No settings file yet - a first run. */
  | 'fresh'
  /** Read cleanly at the current schema version. */
  | 'loaded'
  /** Read at an older schema and migrated forward. */
  | 'migrated'
  /** The primary file was unusable; the previous good copy was used instead. */
  | 'recoveredFromBackup'
  /** Both copies were unusable; the wreckage was set aside, not deleted. */
  | 'quarantined';

export interface StoreHealth {
  status: LoadStatus;
  /** Where a corrupt file was moved to, so the UI can point a user at it. */
  quarantinedTo: string | null;
  /** Last write failure. Non-null means the in-memory state is ahead of disk. */
  writeError: { code: string; message: string } | null;
}

interface Parsed {
  settings: Settings;
  migrated: boolean;
}

interface PendingUpdate {
  mutate: (draft: Settings) => unknown;
  resolve: (value: unknown) => void;
  reject: (error: unknown) => void;
}

/**
 * Parse and migrate, or throw. Returning a blank default for anything
 * unexpected is what turns a stray byte into "all your settings are gone", so
 * this function is deliberately loud and its caller decides recovery policy.
 */
function parse(text: string): Parsed {
  let raw: unknown;
  try {
    raw = JSON.parse(text);
  } catch (cause) {
    fail('stateCorrupt', 'settings file is not valid JSON', {
      reason: cause instanceof Error ? cause.message : String(cause)
    });
  }
  if (typeof raw !== 'object' || raw === null || Array.isArray(raw)) {
    fail('stateCorrupt', 'settings file is not a JSON object');
  }

  let current = raw as Record<string, unknown>;
  // Absent schema field means the original upstream layout, which is version 1.
  const found = typeof current['schema'] === 'number' ? current['schema'] : 1;

  if (found > SCHEMA_VERSION) {
    // A newer build wrote this. Migrating backwards would silently discard
    // whatever it added, so refuse and leave the file untouched instead.
    fail('stateVersionAhead', 'settings were written by a newer version', {
      found,
      supported: SCHEMA_VERSION
    });
  }

  let version = found;
  while (version < SCHEMA_VERSION) {
    const step = MIGRATIONS[version];
    if (!step) fail('stateCorrupt', 'no migration path', { from: version, to: SCHEMA_VERSION });
    current = step(current);
    const next = typeof current['schema'] === 'number' ? current['schema'] : version + 1;
    if (next <= version) fail('stateCorrupt', 'migration did not advance the schema', { version });
    version = next;
  }

  return { settings: sanitize(current), migrated: found !== SCHEMA_VERSION };
}

/**
 * The one owner of the settings file.
 *
 * Reads are synchronous against an in-memory copy. Writes go through update(),
 * which applies mutations under a single writer and coalesces those arriving
 * together into one durable write - so twenty concurrent scan handlers each
 * doing `state.scans[key] = result` cannot lose nineteen of those writes,
 * which is exactly what a read-modify-write per handler does.
 */
export class SettingsStore {
  #settings: Settings;
  #health: StoreHealth;
  #pending: PendingUpdate[] = [];
  #flushing: Promise<void> | null = null;
  // Declared explicitly rather than as a constructor parameter property:
  // Node's strip-only TypeScript mode erases types but never generates code,
  // so `private readonly file: string` in the parameter list will not run.
  readonly #file: string;

  private constructor(file: string, settings: Settings, health: StoreHealth) {
    this.#file = file;
    this.#settings = settings;
    this.#health = health;
  }

  private static backupPath(file: string): string {
    return `${file}.bak`;
  }

  static open(file: string): SettingsStore {
    const primary = readFileOrNull(file);
    if (primary === null) {
      return new SettingsStore(file, defaults(), {
        status: 'fresh',
        quarantinedTo: null,
        writeError: null
      });
    }

    try {
      const { settings, migrated } = parse(primary);
      return new SettingsStore(file, settings, {
        status: migrated ? 'migrated' : 'loaded',
        quarantinedTo: null,
        writeError: null
      });
    } catch (cause) {
      // stateVersionAhead is not corruption - the file is fine and belongs to a
      // newer build. Never quarantine it; let the caller show the mismatch.
      if (cause instanceof AppError && cause.code === 'stateVersionAhead') throw cause;

      const backup = readFileOrNull(SettingsStore.backupPath(file));
      if (backup !== null) {
        try {
          const { settings } = parse(backup);
          return new SettingsStore(file, settings, {
            status: 'recoveredFromBackup',
            quarantinedTo: null,
            writeError: null
          });
        } catch {
          /* the backup is unusable too; fall through to quarantine */
        }
      }

      // Set the wreckage aside under a timestamped name. It may be the only
      // copy of a hand-curated library, so it is never deleted or overwritten.
      const stamp = new Date().toISOString().replace(/[:.]/g, '-');
      const quarantine = `${file}.corrupt-${stamp}`;
      try {
        fs.renameSync(file, quarantine);
      } catch {
        return new SettingsStore(file, defaults(), {
          status: 'quarantined',
          quarantinedTo: null,
          writeError: null
        });
      }
      return new SettingsStore(file, defaults(), {
        status: 'quarantined',
        quarantinedTo: quarantine,
        writeError: null
      });
    }
  }

  /** The current settings. The only way to change them is update(). */
  get(): Readonly<Settings> {
    return this.#settings;
  }

  health(): Readonly<StoreHealth> {
    return this.#health;
  }

  /**
   * Mutate under the write lock and persist. The draft handed to mutate() is a
   * private copy, so a throwing mutator leaves the live settings untouched.
   *
   * Mutations that arrive together are coalesced into one write. A library
   * scan produces one of these per game, and writing the whole settings file
   * once per result means a hundred-game library does a hundred
   * write-flush-replace cycles to save a hundred small facts - which is slow,
   * and on Windows invites the antivirus to hold the file mid-replace. The
   * batch is applied in arrival order and persisted once.
   */
  update<T>(mutate: (draft: Settings) => T): Promise<T> {
    return new Promise<T>((resolve, reject) => {
      this.#pending.push({
        mutate,
        resolve: resolve as (value: unknown) => void,
        reject
      });
      this.#schedule();
    });
  }

  #schedule(): void {
    if (this.#flushing) return;
    this.#flushing = (async () => {
      // Yield once so every caller in this turn joins the same batch.
      await Promise.resolve();
      while (this.#pending.length > 0) {
        await this.#flush(this.#pending.splice(0, this.#pending.length));
      }
      this.#flushing = null;
    })();
  }

  async #flush(batch: PendingUpdate[]): Promise<void> {
    let draft = structuredClone(this.#settings);
    const outcomes: { entry: PendingUpdate; ok: boolean; value?: unknown; error?: unknown }[] = [];

    for (const entry of batch) {
      // Each mutator runs against its own copy, so one that throws part-way
      // through cannot leave the batch holding half of its changes.
      const attempt = structuredClone(draft);
      try {
        const value = entry.mutate(attempt);
        draft = attempt;
        outcomes.push({ entry, ok: true, value });
      } catch (error) {
        outcomes.push({ entry, ok: false, error });
      }
    }

    const applied = outcomes.filter((outcome) => outcome.ok);
    if (applied.length === 0) {
      for (const outcome of outcomes) outcome.entry.reject(outcome.error);
      return;
    }

    draft.schema = SCHEMA_VERSION;
    const next = sanitize(draft);
    try {
      await this.persist(next);
      this.#settings = next;
    } catch (error) {
      // The batch did not reach disk, so everyone whose mutation it carried
      // must hear about it - not just whoever happened to be last.
      for (const outcome of outcomes) outcome.entry.reject(outcome.ok ? error : outcome.error);
      return;
    }
    for (const outcome of outcomes) {
      if (outcome.ok) outcome.entry.resolve(outcome.value);
      else outcome.entry.reject(outcome.error);
    }
  }

  private async persist(next: Settings): Promise<void> {
    try {
      // Keep the last known-good copy before replacing it. This is the file
      // open() falls back to, and it is what makes a torn write survivable.
      try {
        await fs.promises.copyFile(this.#file, SettingsStore.backupPath(this.#file));
      } catch (cause) {
        if (errnoOf(cause) !== 'ENOENT') throw cause;
      }
      await writeJsonAtomic(this.#file, next);
      this.#health = { ...this.#health, writeError: null };
    } catch (cause) {
      // Surface it. Swallowing this is how a read-only or full disk becomes a
      // silent, permanent loss of every setting the user changes afterwards.
      const error = new AppError('stateUnwritable', 'could not save settings', {
        file: this.#file,
        reason: cause instanceof Error ? cause.message : String(cause)
      });
      this.#health = {
        ...this.#health,
        writeError: { code: error.code, message: error.message }
      };
      throw error;
    }
  }

  /** Wait for every queued write to settle. Used on shutdown and in tests. */
  async drain(): Promise<void> {
    // A flush can enqueue the next batch, so keep waiting until it is idle.
    while (this.#flushing) await this.#flushing;
  }

  /** How many writes are queued. Non-zero at shutdown means call drain(). */
  get queued(): number {
    return this.#pending.length;
  }
}
