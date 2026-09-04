import type { ArtRecord, ScanRecord, Settings } from './schema.ts';
import { defaults } from './schema.ts';

/**
 * Persisted settings are untrusted input: an older build wrote them, a newer
 * one may have, or a person opened the file in an editor. Every field is
 * therefore coerced against a default rather than believed.
 *
 * Dropping one malformed field must never cost the user the other forty, which
 * is the failure this whole module exists to prevent.
 */

const isObject = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

const str = (v: unknown, fallback: string): string => (typeof v === 'string' ? v : fallback);
const bool = (v: unknown, fallback: boolean): boolean => (typeof v === 'boolean' ? v : fallback);
const num = (v: unknown, fallback: number): number =>
  typeof v === 'number' && Number.isFinite(v) ? v : fallback;
const nullableStr = (v: unknown): string | null => (typeof v === 'string' ? v : null);

/** Absolute paths only, de-duplicated case-insensitively, order preserved. */
function pathList(v: unknown): string[] {
  if (!Array.isArray(v)) return [];
  const seen = new Set<string>();
  const out: string[] = [];
  for (const item of v) {
    if (typeof item !== 'string' || item.length === 0 || item.includes('\0')) continue;
    const key = item.toLowerCase();
    if (seen.has(key)) continue;
    seen.add(key);
    out.push(item);
  }
  return out;
}

function stringMap(v: unknown): Record<string, string> {
  if (!isObject(v)) return {};
  const out: Record<string, string> = {};
  for (const [key, value] of Object.entries(v)) if (typeof value === 'string') out[key] = value;
  return out;
}

function bitness(v: unknown): 32 | 64 | null {
  return v === 32 || v === 64 ? v : null;
}

function scanRecord(v: unknown): ScanRecord | null {
  if (!isObject(v)) return null;
  // A record with no generation stamp predates the field and cannot be trusted
  // to match current detection rules; treat it as absent so it is rescanned.
  if (typeof v['rules'] !== 'number') return null;
  return {
    ok: bool(v['ok'], false),
    api: nullableStr(v['api']),
    apiLabel: nullableStr(v['apiLabel']),
    bitness: bitness(v['bitness']),
    exe: nullableStr(v['exe']),
    dlssVersion: nullableStr(v['dlssVersion']),
    routes: Array.isArray(v['routes']) ? v['routes'].filter((r): r is string => typeof r === 'string') : [],
    reason: nullableStr(v['reason']),
    scannedAt: num(v['scannedAt'], 0),
    rules: v['rules']
  };
}

function artRecord(v: unknown): ArtRecord | null {
  if (!isObject(v) || typeof v['rules'] !== 'number') return null;
  const record: ArtRecord = {
    appid: typeof v['appid'] === 'number' ? v['appid'] : null,
    cover: nullableStr(v['cover']),
    hero: nullableStr(v['hero']),
    rules: v['rules'],
    fetchedAt: num(v['fetchedAt'], 0)
  };
  if (v['miss'] === true) record.miss = true;
  return record;
}

function recordMap<T>(v: unknown, each: (value: unknown) => T | null): Record<string, T> {
  if (!isObject(v)) return {};
  const out: Record<string, T> = {};
  for (const [key, value] of Object.entries(v)) {
    const parsed = each(value);
    if (parsed !== null) out[key] = parsed;
  }
  return out;
}

function recents(v: unknown): { dir: string; at: number }[] {
  if (!Array.isArray(v)) return [];
  return v
    .filter(isObject)
    .filter((row) => typeof row['dir'] === 'string')
    .map((row) => ({ dir: row['dir'] as string, at: num(row['at'], 0) }))
    .sort((a, b) => b.at - a.at)
    .slice(0, 24);
}

function addons(v: unknown): Settings['addons'] {
  if (!Array.isArray(v)) return [];
  return v
    .filter(isObject)
    .filter((row) => typeof row['path'] === 'string')
    .map((row) => ({
      path: row['path'] as string,
      name: nullableStr(row['name']),
      notes: Array.isArray(row['notes'])
        ? row['notes'].filter((n): n is string => typeof n === 'string')
        : [],
      enabled: bool(row['enabled'], false)
    }));
}

const THEMES = new Set<Settings['theme']>(['light', 'dark', 'system']);

export function sanitize(input: unknown): Settings {
  const base = defaults();
  if (!isObject(input)) return base;
  const theme = input['theme'];
  return {
    schema: base.schema,
    theme: typeof theme === 'string' && THEMES.has(theme as Settings['theme'])
      ? (theme as Settings['theme'])
      : base.theme,
    // A language tag, not free text: it indexes a catalogue and reaches the DOM.
    lang: /^[a-z]{2,3}(-[A-Za-z0-9]{2,8})*$/.test(str(input['lang'], '')) ? (input['lang'] as string) : base.lang,
    groupGamesByStore: bool(input['groupGamesByStore'], base.groupGamesByStore),
    autoScanDrives: bool(input['autoScanDrives'], base.autoScanDrives),
    folders: pathList(input['folders']),
    excludedRoots: pathList(input['excludedRoots']),
    manual: pathList(input['manual']),
    hidden: pathList(input['hidden']),
    posters: stringMap(input['posters']),
    scans: recordMap(input['scans'], scanRecord),
    art: recordMap(input['art'], artRecord),
    recents: recents(input['recents']),
    addons: addons(input['addons'])
  };
}
