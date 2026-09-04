import fs from 'node:fs';
import { PeFile } from './reader.ts';

/**
 * Everything the scanner needs to know about one binary, gathered in a single
 * open, plus a cache so that asking again is free.
 *
 * Rescanning a library is the common case - the app does it on every launch,
 * after every install, and whenever a folder is added - and almost nothing has
 * changed between one scan and the next. Re-reading every header and
 * re-scanning every section each time is the bulk of that work, and all of it
 * is avoidable: a file whose size and modification time are unchanged cannot
 * have different imports than it did a minute ago.
 */

export interface PeSummary {
  bitness: 32 | 64;
  machine: number;
  /** Imported and delay-imported DLL names, lower-cased. */
  imports: string[];
  fileVersion: string | null;
  /** Which of the requested markers were present. */
  markers: string[];
  /** Which of the requested version-resource strings were present. */
  versionStrings: string[];
  /** Which of the requested section probes matched. */
  probes: string[];
}

export interface SummaryRequest {
  /** ASCII entry-point / DLL-name strings to look for in mapped sections. */
  markers?: readonly string[];
  /** Vendor strings to look for in the version resource. */
  versionStrings?: readonly string[];
  /** Named byte probes, e.g. the ReShade add-on loader signature. */
  probes?: Readonly<Record<string, string>>;
  /**
   * Bumped whenever the requested sets change, so entries cached under an
   * older question are not answered with a stale result.
   */
  rules: number;
}

export function summarize(file: string, request: SummaryRequest): PeSummary | null {
  return PeFile.with(
    file,
    (pe) => {
      const markers = request.markers?.length ? [...pe.findMarkers(request.markers)] : [];
      const versionStrings = (request.versionStrings ?? []).filter((text) =>
        pe.versionMentions(text)
      );
      const probes = Object.entries(request.probes ?? {})
        .filter(([, needle]) => pe.containsBytes(needle))
        .map(([name]) => name);

      return {
        bitness: pe.bitness,
        machine: pe.machine,
        imports: pe.imports(),
        fileVersion: pe.fileVersion(),
        markers,
        versionStrings,
        probes
      } satisfies PeSummary;
    },
    null
  );
}

interface Entry {
  size: number;
  mtimeMs: number;
  rules: number;
  /** null records "this is not a parseable PE", which is worth remembering. */
  summary: PeSummary | null;
}

export interface PeCacheStats {
  hits: number;
  misses: number;
  evictions: number;
}

/**
 * Identity is (path, size, modification time). A game update changes at least
 * one of the latter two, so a patched executable is always re-read, while an
 * untouched one never is.
 */
export class PeCache {
  readonly #entries = new Map<string, Entry>();
  #stats: PeCacheStats = { hits: 0, misses: 0, evictions: 0 };

  constructor(saved: Readonly<Record<string, Entry>> = {}) {
    for (const [key, entry] of Object.entries(saved)) {
      if (typeof entry?.size === 'number' && typeof entry?.mtimeMs === 'number') {
        this.#entries.set(key, entry);
      }
    }
  }

  get stats(): Readonly<PeCacheStats> {
    return this.#stats;
  }

  get size(): number {
    return this.#entries.size;
  }

  summarize(file: string, request: SummaryRequest): PeSummary | null {
    let stat: fs.Stats;
    try {
      stat = fs.statSync(file);
    } catch {
      // Gone since the directory walk. Forget any entry for it.
      this.#entries.delete(file.toLowerCase());
      return null;
    }
    if (!stat.isFile()) return null;

    const key = file.toLowerCase();
    const cached = this.#entries.get(key);
    if (
      cached &&
      cached.size === stat.size &&
      cached.mtimeMs === stat.mtimeMs &&
      cached.rules === request.rules
    ) {
      this.#stats = { ...this.#stats, hits: this.#stats.hits + 1 };
      return cached.summary;
    }
    if (cached) this.#stats = { ...this.#stats, evictions: this.#stats.evictions + 1 };

    const summary = summarize(file, request);
    this.#entries.set(key, {
      size: stat.size,
      mtimeMs: stat.mtimeMs,
      rules: request.rules,
      summary
    });
    this.#stats = { ...this.#stats, misses: this.#stats.misses + 1 };
    return summary;
  }

  /** Drop entries for files that no longer exist, so the cache cannot grow forever. */
  prune(): number {
    let removed = 0;
    for (const key of [...this.#entries.keys()]) {
      if (!fs.existsSync(key)) {
        this.#entries.delete(key);
        removed += 1;
      }
    }
    return removed;
  }

  toJSON(): Record<string, Entry> {
    return Object.fromEntries(this.#entries);
  }
}
