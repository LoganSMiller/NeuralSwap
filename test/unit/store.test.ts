import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { test } from 'node:test';
import { SettingsStore } from '../../src/main/state/store.ts';
import { SCHEMA_VERSION } from '../../src/main/state/schema.ts';
import { AppError } from '../../src/shared/errors.ts';
import { scratch } from '../fixtures/tmp.ts';

const root = scratch('store');
let counter = 0;

/** A fresh settings path, optionally pre-seeded with raw file contents. */
function seed(contents?: string, backup?: string): string {
  const file = path.join(root, `settings${counter++}.json`);
  if (contents !== undefined) fs.writeFileSync(file, contents);
  if (backup !== undefined) fs.writeFileSync(`${file}.bak`, backup);
  return file;
}

test('a missing file is a first run, not an error', () => {
  const store = SettingsStore.open(seed());
  assert.equal(store.health().status, 'fresh');
  assert.equal(store.get().lang, 'en');
  // Full-drive scanning must stay opt-in.
  assert.equal(store.get().autoScanDrives, false);
});

test('a valid file loads as written', async () => {
  const file = seed();
  const first = SettingsStore.open(file);
  await first.update((draft) => {
    draft.lang = 'ja';
    draft.folders.push('D:\\Games');
  });

  const second = SettingsStore.open(file);
  assert.equal(second.health().status, 'loaded');
  assert.equal(second.get().lang, 'ja');
  assert.deepEqual(second.get().folders, ['D:\\Games']);
});

test('a v1 upstream file migrates instead of being discarded', () => {
  // The shape the original app wrote: no schema field, a single `addon` path,
  // and `addonFiles` for hand-added builds.
  const file = seed(
    JSON.stringify({
      theme: 'dark',
      lang: 'de',
      folders: ['E:\\SteamLibrary'],
      hidden: ['E:\\SteamLibrary\\Skyrim'],
      addon: 'C:\\builds\\renodx-dlss.addon64',
      addonFiles: [{ path: 'C:\\builds\\other.addon64', name: 'Other' }],
      posters: { abc123: 'C:\\posters\\abc123.png' }
    })
  );
  const store = SettingsStore.open(file);
  assert.equal(store.health().status, 'migrated');
  assert.equal(store.get().schema, SCHEMA_VERSION);
  assert.equal(store.get().theme, 'dark');
  assert.equal(store.get().lang, 'de');
  assert.deepEqual(store.get().folders, ['E:\\SteamLibrary']);
  assert.deepEqual(store.get().posters, { abc123: 'C:\\posters\\abc123.png' });
  // Both the single `addon` and the `addonFiles` catalogue survive as one list.
  assert.deepEqual(
    store.get().addons.map((a) => a.path).sort(),
    ['C:\\builds\\other.addon64', 'C:\\builds\\renodx-dlss.addon64']
  );
  const byPath = new Map(store.get().addons.map((a) => [a.path, a]));
  // The catalogued build keeps its name but was never switched on...
  assert.equal(byPath.get('C:\\builds\\other.addon64')?.name, 'Other');
  assert.equal(byPath.get('C:\\builds\\other.addon64')?.enabled, false);
  // ...while the legacy single `addon` was, so it must stay enabled.
  assert.equal(byPath.get('C:\\builds\\renodx-dlss.addon64')?.enabled, true);
});

test('a corrupt file falls back to the backup copy', () => {
  const good = JSON.stringify({ schema: SCHEMA_VERSION, lang: 'fr', folders: ['F:\\Games'] });
  const file = seed('{ this is not json', good);
  const store = SettingsStore.open(file);
  assert.equal(store.health().status, 'recoveredFromBackup');
  assert.equal(store.get().lang, 'fr');
  assert.deepEqual(store.get().folders, ['F:\\Games']);
});

test('a corrupt file with no usable backup is quarantined, never deleted', () => {
  const file = seed('{ truncated', 'also broken {');
  const store = SettingsStore.open(file);
  assert.equal(store.health().status, 'quarantined');

  const quarantine = store.health().quarantinedTo;
  assert.ok(quarantine, 'expected a quarantine path');
  // The user's only copy of a hand-curated library may be in there.
  assert.equal(fs.readFileSync(quarantine as string, 'utf8'), '{ truncated');
  assert.equal(fs.existsSync(file), false);
  assert.equal(store.get().lang, 'en');
});

test('settings written by a newer build are refused, not downgraded', () => {
  const file = seed(JSON.stringify({ schema: SCHEMA_VERSION + 5, lang: 'it', futureField: 1 }));
  assert.throws(
    () => SettingsStore.open(file),
    (cause: unknown) => cause instanceof AppError && cause.code === 'stateVersionAhead'
  );
  // Critically, the file is left exactly as it was for the newer build.
  const onDisk = JSON.parse(fs.readFileSync(file, 'utf8')) as Record<string, unknown>;
  assert.equal(onDisk['futureField'], 1);
  assert.equal(onDisk['schema'], SCHEMA_VERSION + 5);
});

test('one malformed field does not cost the user the others', () => {
  const file = seed(
    JSON.stringify({
      schema: SCHEMA_VERSION,
      lang: 'es',
      folders: ['G:\\Games'],
      theme: 'chartreuse',
      groupGamesByStore: 'yes please',
      recents: 'not an array',
      scans: { abc: { ok: true, rules: 7, scannedAt: 5 }, bad: 'nope' }
    })
  );
  const store = SettingsStore.open(file);
  assert.equal(store.health().status, 'loaded');
  assert.equal(store.get().lang, 'es');
  assert.deepEqual(store.get().folders, ['G:\\Games']);
  // Bad fields fall back to defaults; good ones are untouched.
  assert.equal(store.get().theme, 'system');
  assert.equal(store.get().groupGamesByStore, true);
  assert.deepEqual(store.get().recents, []);
  assert.equal(store.get().scans['abc']?.rules, 7);
  assert.equal(store.get().scans['bad'], undefined);
});

test('a cached scan with no rules stamp is dropped so it is rescanned', () => {
  const file = seed(
    JSON.stringify({ schema: SCHEMA_VERSION, scans: { old: { ok: true, api: 'dxgi' } } })
  );
  const store = SettingsStore.open(file);
  assert.equal(store.get().scans['old'], undefined);
});

test('concurrent updates all survive', async () => {
  const file = seed();
  const store = SettingsStore.open(file);

  // This is the failure mode the store exists to remove: many handlers each
  // doing read-modify-write on the same object. Serialised through update(),
  // every one of the fifty must be present afterwards.
  await Promise.all(
    Array.from({ length: 50 }, (_, i) =>
      store.update((draft) => {
        draft.scans[`game${i}`] = {
          ok: true,
          api: 'dxgi',
          apiLabel: 'DirectX 12',
          bitness: 64,
          exe: `game${i}.exe`,
          dlssVersion: null,
          routes: ['native'],
          reason: null,
          scannedAt: i,
          rules: 1
        };
      })
    )
  );
  await store.drain();

  assert.equal(Object.keys(store.get().scans).length, 50);
  const reopened = SettingsStore.open(file);
  assert.equal(Object.keys(reopened.get().scans).length, 50);
});

test('updates arriving together are coalesced into a single write', async () => {
  const file = seed();
  const store = SettingsStore.open(file);

  await Promise.all(
    Array.from({ length: 40 }, (_, i) =>
      store.update((draft) => {
        draft.folders.push(`D:\\Games\\game${i}`);
      })
    )
  );
  await store.drain();

  assert.equal(store.get().folders.length, 40);
  // Each write copies the current file aside as `.bak` first. On a fresh
  // settings file there is nothing to copy, so a single `.bak` appearing would
  // mean a second write happened - forty writes to record forty small facts is
  // what makes a library scan slow, and on Windows it is what invites the
  // antivirus to hold the file mid-replace.
  assert.equal(fs.existsSync(`${file}.bak`), false, 'expected exactly one write for the batch');
});

test('a coalesced batch reports each caller its own result', async () => {
  const file = seed();
  const store = SettingsStore.open(file);

  const [first, second, third] = await Promise.all([
    store.update((draft) => {
      draft.lang = 'fi';
      return 'first';
    }),
    store.update(() => 42),
    store.update((draft) => {
      draft.theme = 'dark';
      return { ok: true };
    })
  ]);

  assert.equal(first, 'first');
  assert.equal(second, 42);
  assert.deepEqual(third, { ok: true });
  assert.equal(store.get().lang, 'fi');
  assert.equal(store.get().theme, 'dark');
});

test('one throwing mutator does not take its batch down with it', async () => {
  const file = seed();
  const store = SettingsStore.open(file);

  const results = await Promise.allSettled([
    store.update((draft) => {
      draft.lang = 'da';
    }),
    store.update(() => {
      throw new Error('this one is broken');
    }),
    store.update((draft) => {
      draft.theme = 'light';
    })
  ]);

  assert.deepEqual(
    results.map((r) => r.status),
    ['fulfilled', 'rejected', 'fulfilled']
  );
  // The good mutations landed; the broken one contributed nothing.
  assert.equal(store.get().lang, 'da');
  assert.equal(store.get().theme, 'light');
});

test('a throwing mutator leaves the live settings untouched', async () => {
  const file = seed();
  const store = SettingsStore.open(file);
  await store.update((draft) => {
    draft.lang = 'pl';
  });

  await assert.rejects(() =>
    store.update((draft) => {
      draft.lang = 'xx';
      throw new Error('mutator failed');
    })
  );
  assert.equal(store.get().lang, 'pl');

  // And the queue is not wedged by the failure.
  await store.update((draft) => {
    draft.lang = 'nl';
  });
  assert.equal(store.get().lang, 'nl');
});

test('an unwritable file surfaces as an error and is recorded in health', async () => {
  // A directory where the settings file should be makes every write fail,
  // which stands in for a read-only profile or a full disk.
  const file = path.join(root, `blocked${counter++}.json`);
  const store = SettingsStore.open(file);
  fs.mkdirSync(file, { recursive: true });

  await assert.rejects(
    () => store.update((draft) => { draft.lang = 'sv'; }),
    (cause: unknown) => cause instanceof AppError && cause.code === 'stateUnwritable'
  );
  // Never silently swallowed: the UI can tell the user their settings are not
  // being saved, which upstream's `catch {}` made impossible.
  assert.equal(store.health().writeError?.code, 'stateUnwritable');
});

test('a previous good copy is kept as the backup on every write', async () => {
  const file = seed();
  const store = SettingsStore.open(file);
  await store.update((draft) => { draft.lang = 'cs'; });
  await store.update((draft) => { draft.lang = 'hu'; });
  await store.drain();

  const backup = JSON.parse(fs.readFileSync(`${file}.bak`, 'utf8')) as { lang: string };
  assert.equal(backup.lang, 'cs');
  assert.equal(store.get().lang, 'hu');
});
