import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { test } from 'node:test';
import { assertSafeRelative } from '../../src/core/fsx/paths.ts';
import { DEFAULT_LIMITS, extractZip } from '../../src/core/zip/extract.ts';
import { PeFile } from '../../src/core/pe/reader.ts';
import { SettingsStore } from '../../src/main/state/store.ts';
import { AppError } from '../../src/shared/errors.ts';
import { scratch } from '../fixtures/tmp.ts';

/**
 * Replays `spec/` against the reference implementation.
 *
 * The vectors were generated from this implementation, so this is not an
 * independent check of correctness - the hand-written tests are that. What it
 * catches is **drift**: change the reference and forget to regenerate, and
 * these fail, which is what stops a reimplementation being held to a
 * specification the original no longer follows.
 *
 * It is also the exact shape a Rust or C# suite should take: load the table,
 * feed the fixture, compare the code.
 */

const specRoot = path.resolve(import.meta.dirname, '../../spec');
const hasSpec = fs.existsSync(path.join(specRoot, 'paths.json'));

const readTable = <T>(rel: string): T =>
  JSON.parse(fs.readFileSync(path.join(specRoot, rel), 'utf8')) as T;

const codeOf = (fn: () => unknown): string => {
  try {
    fn();
    return 'ok';
  } catch (error) {
    if (error instanceof AppError) return error.code;
    throw error;
  }
};

test('spec/ has been generated', (t) => {
  if (!hasSpec) {
    t.skip('run: node scripts/export-vectors.mjs');
    return;
  }
  assert.ok(fs.existsSync(path.join(specRoot, 'README.md')));
});

test('path vectors match the reference', (t) => {
  if (!hasSpec) return t.skip('no spec');
  const table = readTable<{
    root: string;
    cases: { rel: string; why: string; expect: string }[];
  }>('paths.json');

  assert.ok(table.cases.length >= 25, 'the table lost cases');
  for (const row of table.cases) {
    assert.equal(
      codeOf(() => assertSafeRelative(row.rel, table.root)),
      row.expect,
      `${JSON.stringify(row.rel)} (${row.why})`
    );
  }
});

test('archive vectors match the reference, and refuse before writing', async (t) => {
  if (!hasSpec) return t.skip('no spec');
  const table = readTable<{ cases: { file: string; why: string; expect: string }[] }>(
    'zip/cases.json'
  );
  const work = scratch('vectors-zip');

  assert.ok(table.cases.length >= 14, 'the table lost cases');
  for (const [index, row] of table.cases.entries()) {
    const dest = path.join(work, `case${index}`);
    let actual: string;
    try {
      await extractZip(path.join(specRoot, row.file), dest, DEFAULT_LIMITS);
      actual = 'ok';
    } catch (error) {
      if (!(error instanceof AppError)) throw error;
      actual = error.code;
    }
    assert.equal(actual, row.expect, `${row.file} (${row.why})`);

    // The stronger half of the guarantee: a refusal must leave nothing behind.
    if (actual !== 'ok' && fs.existsSync(dest)) {
      const written: string[] = [];
      const walk = (dir: string): void => {
        for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
          if (entry.isDirectory()) walk(path.join(dir, entry.name));
          else written.push(path.join(dir, entry.name));
        }
      };
      walk(dest);
      assert.deepEqual(written, [], `${row.file} wrote files despite failing`);
    }
  }
});

test('PE vectors match the reference', (t) => {
  if (!hasSpec) return t.skip('no spec');
  const table = readTable<{
    markers: string[];
    cases: {
      file: string;
      why: string;
      sizeBytes: number;
      expect: {
        parses: boolean;
        bitness?: 32 | 64;
        machine?: number;
        imports?: string[];
        markers?: string[];
      };
    }[];
  }>('pe/cases.json');

  for (const row of table.cases) {
    const file = path.join(specRoot, row.file);
    const observed = PeFile.with(
      file,
      (pe) => ({
        parses: true,
        bitness: pe.bitness,
        machine: pe.machine,
        imports: [...pe.imports()].sort(),
        markers: [...pe.findMarkers(table.markers)].sort(),
        bytesRead: pe.bytesRead
      }),
      null
    );

    if (!row.expect.parses) {
      assert.equal(observed, null, `${row.file} should not parse (${row.why})`);
      continue;
    }
    assert.ok(observed, `${row.file} failed to parse (${row.why})`);
    assert.equal(observed.bitness, row.expect.bitness, `${row.file} bitness`);
    assert.equal(observed.machine, row.expect.machine, `${row.file} machine`);
    assert.deepEqual(observed.imports, row.expect.imports, `${row.file} imports`);
    assert.deepEqual(observed.markers, row.expect.markers, `${row.file} markers`);

    // The overlay case is the one where IO volume is itself the guarantee.
    if (row.file.includes('overlay')) {
      assert.ok(
        observed.bytesRead < row.sizeBytes / 4,
        `${row.file}: read ${observed.bytesRead} of ${row.sizeBytes} - the overlay is being scanned`
      );
    }
  }
});

test('settings vectors match the reference', (t) => {
  if (!hasSpec) return t.skip('no spec');
  const table = readTable<{
    cases: { file: string; why: string; expect: { status: string; code?: string } }[];
  }>('settings/cases.json');
  const work = scratch('vectors-settings');

  for (const [index, row] of table.cases.entries()) {
    // Opening a corrupt file quarantines it by renaming, so work on a copy and
    // leave the vector intact for the next implementation to read.
    const dir = path.join(work, `case${index}`);
    fs.mkdirSync(dir, { recursive: true });
    const copy = path.join(dir, 'settings.json');
    fs.copyFileSync(path.join(specRoot, row.file), copy);

    if (row.expect.status === 'refused') {
      assert.throws(
        () => SettingsStore.open(copy),
        (error: unknown) => error instanceof AppError && error.code === row.expect.code,
        `${row.file} (${row.why})`
      );
      // Refusing must not touch the file: it belongs to the newer build.
      assert.equal(
        fs.readFileSync(copy, 'utf8'),
        fs.readFileSync(path.join(specRoot, row.file), 'utf8'),
        `${row.file} was modified despite being refused`
      );
      continue;
    }

    const store = SettingsStore.open(copy);
    assert.equal(store.health().status, row.expect.status, `${row.file} (${row.why})`);

    if (row.expect.status === 'quarantined') {
      // Set aside, never destroyed.
      const quarantine = store.health().quarantinedTo;
      assert.ok(quarantine, `${row.file}: nothing was quarantined`);
      assert.equal(
        fs.readFileSync(quarantine as string, 'utf8'),
        fs.readFileSync(path.join(specRoot, row.file), 'utf8'),
        `${row.file}: the quarantined copy does not match the original`
      );
    }
  }
});
