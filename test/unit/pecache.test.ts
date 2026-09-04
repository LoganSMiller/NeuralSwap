import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { test } from 'node:test';
import { PeCache, summarize, type SummaryRequest } from '../../src/core/pe/summary.ts';
import { buildPe } from '../fixtures/pebuild.ts';
import { scratch } from '../fixtures/tmp.ts';

const root = scratch('pecache');

const REQUEST: SummaryRequest = {
  markers: ['D3D12CreateDevice', 'vkCreateInstance'],
  versionStrings: ['ReShade'],
  probes: { addonLoader: 'Searching for add-ons' },
  rules: 1
};

function write(name: string, imports: string[], body?: Buffer): string {
  const file = path.join(root, name);
  const spec = body
    ? { bitness: 64 as const, imports, sections: [{ name: '.text', data: body }] }
    : { bitness: 64 as const, imports };
  fs.writeFileSync(file, buildPe(spec));
  return file;
}

test('a summary gathers imports, markers and probes in one pass', () => {
  const body = Buffer.alloc(8192, 0x20);
  body.write('D3D12CreateDevice', 100, 'latin1');
  body.write('Searching for add-ons', 2000, 'latin1');
  const file = write('summary.exe', ['d3d12.dll', 'kernel32.dll'], body);

  const summary = summarize(file, REQUEST);
  assert.ok(summary);
  assert.equal(summary.bitness, 64);
  assert.deepEqual(summary.imports.sort(), ['d3d12.dll', 'kernel32.dll']);
  assert.deepEqual(summary.markers, ['D3D12CreateDevice']);
  assert.deepEqual(summary.probes, ['addonLoader']);
  assert.deepEqual(summary.versionStrings, []);
});

test('an unchanged file is answered from cache', () => {
  const file = write('cached.exe', ['d3d11.dll']);
  const cache = new PeCache();

  const first = cache.summarize(file, REQUEST);
  const second = cache.summarize(file, REQUEST);
  assert.deepEqual(first, second);
  assert.equal(cache.stats.misses, 1);
  assert.equal(cache.stats.hits, 1);
});

test('a patched file is re-read', () => {
  const file = write('patched.exe', ['d3d11.dll']);
  const cache = new PeCache();
  assert.deepEqual(cache.summarize(file, REQUEST)?.imports, ['d3d11.dll']);

  // A game update changes size and modification time, which is exactly the
  // signal the cache keys on.
  fs.writeFileSync(file, buildPe({ bitness: 64, imports: ['d3d12.dll', 'dxgi.dll'] }));
  fs.utimesSync(file, new Date(), new Date(Date.now() + 5000));

  assert.deepEqual(cache.summarize(file, REQUEST)?.imports.sort(), ['d3d12.dll', 'dxgi.dll']);
  assert.equal(cache.stats.evictions, 1);
});

test('changing the question invalidates entries cached under the old one', () => {
  const file = write('rules.exe', ['dxgi.dll']);
  const cache = new PeCache();
  cache.summarize(file, REQUEST);
  assert.equal(cache.stats.misses, 1);

  // A new detection generation must not be served last generation's answer.
  cache.summarize(file, { ...REQUEST, rules: 2 });
  assert.equal(cache.stats.misses, 2);
});

test('a non-PE file caches its negative result', () => {
  const file = path.join(root, 'notpe.bin');
  fs.writeFileSync(file, Buffer.alloc(4096, 0x7f));
  const cache = new PeCache();

  assert.equal(cache.summarize(file, REQUEST), null);
  assert.equal(cache.summarize(file, REQUEST), null);
  // Remembering "this is not a PE" is what stops a folder full of data files
  // being re-examined on every single scan.
  assert.equal(cache.stats.misses, 1);
  assert.equal(cache.stats.hits, 1);
});

test('a deleted file is forgotten rather than reported stale', () => {
  const file = write('gone.exe', ['dxgi.dll']);
  const cache = new PeCache();
  assert.ok(cache.summarize(file, REQUEST));
  assert.equal(cache.size, 1);

  fs.unlinkSync(file);
  assert.equal(cache.summarize(file, REQUEST), null);
  assert.equal(cache.size, 0);
});

test('the cache survives a round-trip through JSON', () => {
  const file = write('persist.exe', ['d3d12.dll']);
  const cache = new PeCache();
  cache.summarize(file, REQUEST);

  const revived = new PeCache(JSON.parse(JSON.stringify(cache)) as Record<string, never>);
  const summary = revived.summarize(file, REQUEST);
  assert.deepEqual(summary?.imports, ['d3d12.dll']);
  // Restored from disk on the next launch, so the first scan is already warm.
  assert.equal(revived.stats.hits, 1);
  assert.equal(revived.stats.misses, 0);
});

test('prune drops entries for files that are gone', () => {
  const keep = write('keep.exe', ['dxgi.dll']);
  const drop = write('drop.exe', ['dxgi.dll']);
  const cache = new PeCache();
  cache.summarize(keep, REQUEST);
  cache.summarize(drop, REQUEST);
  assert.equal(cache.size, 2);

  fs.unlinkSync(drop);
  assert.equal(cache.prune(), 1);
  assert.equal(cache.size, 1);
});

test('a garbage persisted cache is ignored, not fatal', () => {
  const cache = new PeCache({
    'c:\\nonsense': { size: 'big' } as never,
    'c:\\also-bad': null as never
  });
  assert.equal(cache.size, 0);
});
