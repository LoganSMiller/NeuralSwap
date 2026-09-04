import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { test } from 'node:test';
import { readFileOrNull, writeFileAtomic, writeJsonAtomic } from '../../src/core/fsx/atomic.ts';
import { scratch } from '../fixtures/tmp.ts';

const root = scratch('atomic');

test('writes create missing parent directories', async () => {
  const file = path.join(root, 'deep', 'nested', 'settings.json');
  await writeJsonAtomic(file, { hello: 'world' });
  assert.deepEqual(JSON.parse(fs.readFileSync(file, 'utf8')), { hello: 'world' });
});

test('a replacement is never visible half-written', async () => {
  const file = path.join(root, 'replace.json');
  await writeJsonAtomic(file, { generation: 1 });

  // Interleave a reader with many writers. Because the replace is a rename,
  // every read must see one complete generation and never a torn mixture.
  let torn = 0;
  let reads = 0;
  const reader = setInterval(() => {
    const text = readFileOrNull(file);
    if (text === null) return;
    reads += 1;
    try {
      JSON.parse(text);
    } catch {
      torn += 1;
    }
  }, 1);

  for (let generation = 2; generation <= 60; generation += 1) {
    await writeJsonAtomic(file, { generation, filler: 'x'.repeat(4096) });
  }
  clearInterval(reader);

  assert.equal(torn, 0, `saw ${torn} torn reads out of ${reads}`);
  assert.ok(reads > 0, 'the reader never ran');
});

test('no temp files are left behind', async () => {
  const file = path.join(root, 'clean.json');
  await writeJsonAtomic(file, { a: 1 });
  await writeJsonAtomic(file, { a: 2 });
  const strays = fs.readdirSync(root).filter((name) => name.includes('.tmp'));
  assert.deepEqual(strays, []);
});

test('a failed write leaves no temp file and no truncated target', async () => {
  const file = path.join(root, 'blocked.json');
  await writeJsonAtomic(file, { keep: 'this' });

  // A directory in place of the temp target is not something we can create, so
  // block the destination itself by making it a directory instead.
  const blocked = path.join(root, 'blocked-dir.json');
  fs.mkdirSync(blocked, { recursive: true });
  await assert.rejects(() => writeJsonAtomic(blocked, { a: 1 }));

  // The unrelated good file is untouched, and nothing was left lying around.
  assert.deepEqual(JSON.parse(fs.readFileSync(file, 'utf8')), { keep: 'this' });
  assert.deepEqual(
    fs.readdirSync(root).filter((name) => name.includes('.tmp')),
    []
  );
});

test('buffers and strings both round-trip', async () => {
  const text = path.join(root, 'text.txt');
  const binary = path.join(root, 'binary.bin');
  await writeFileAtomic(text, 'plain text\n');
  await writeFileAtomic(binary, Buffer.from([0, 1, 2, 253, 254, 255]));
  assert.equal(fs.readFileSync(text, 'utf8'), 'plain text\n');
  assert.deepEqual([...fs.readFileSync(binary)], [0, 1, 2, 253, 254, 255]);
});

test('readFileOrNull distinguishes absent from unreadable', () => {
  assert.equal(readFileOrNull(path.join(root, 'nope.json')), null);
  // A directory is present but not a file: that is an error, not an absence,
  // and conflating the two is how a real failure becomes a silent reset.
  const dir = path.join(root, 'a-directory');
  fs.mkdirSync(dir, { recursive: true });
  assert.throws(() => readFileOrNull(dir));
});
