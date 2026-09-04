import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { test } from 'node:test';
import { DEFAULT_LIMITS, extractZip } from '../../src/core/zip/extract.ts';
import { AppError } from '../../src/shared/errors.ts';
import { buildZip, type BuildEntry } from '../fixtures/zipbuild.ts';
import { scratch } from '../fixtures/tmp.ts';

const root = scratch('zip');
let counter = 0;

/** Write an archive and try to extract it; returns the error code or 'ok'. */
async function attempt(entries: BuildEntry[], limits = DEFAULT_LIMITS): Promise<{ code: string; dest: string }> {
  const id = `case${counter++}`;
  const archive = path.join(root, `${id}.zip`);
  const dest = path.join(root, id);
  fs.writeFileSync(archive, buildZip(entries));
  try {
    await extractZip(archive, dest, limits);
    return { code: 'ok', dest };
  } catch (cause) {
    return { code: cause instanceof AppError ? cause.code : `unexpected:${String(cause)}`, dest };
  }
}

test('a normal archive round-trips, deflated and stored', async () => {
  const { code, dest } = await attempt([
    { name: 'readme.txt', data: 'hello world' },
    { name: 'bin/tool.dll', data: Buffer.alloc(4096, 0x41), method: 8 },
    { name: 'bin/raw.bin', data: Buffer.alloc(1024, 0x42), method: 0 },
    { name: 'empty/', directory: true }
  ]);
  assert.equal(code, 'ok');
  assert.equal(fs.readFileSync(path.join(dest, 'readme.txt'), 'utf8'), 'hello world');
  assert.equal(fs.statSync(path.join(dest, 'bin', 'tool.dll')).size, 4096);
  assert.equal(fs.statSync(path.join(dest, 'bin', 'raw.bin')).size, 1024);
  assert.ok(fs.statSync(path.join(dest, 'empty')).isDirectory());
});

test('an empty file entry is written as an empty file', async () => {
  const { code, dest } = await attempt([{ name: 'blank.txt', data: '' }]);
  assert.equal(code, 'ok');
  assert.equal(fs.statSync(path.join(dest, 'blank.txt')).size, 0);
});

test('a traversal entry is refused and writes nothing', async () => {
  const { code, dest } = await attempt([
    { name: 'good.txt', data: 'fine' },
    { name: '../escaped.txt', data: 'pwned' }
  ]);
  assert.equal(code, 'unsafePath');
  // Validation happens before any write, so even the innocent sibling entry
  // must not have landed - a half-extracted hostile archive is still hostile.
  assert.equal(fs.existsSync(dest), false);
  assert.equal(fs.existsSync(path.join(root, 'escaped.txt')), false);
});

test('a backslash traversal entry is refused', async () => {
  // 7-Zip and older Windows archivers write backslash separators, so this is
  // the same attack wearing the other separator.
  const { code } = await attempt([{ name: '..\\escaped.txt', data: 'pwned' }]);
  assert.equal(code, 'unsafePath');
  assert.equal(fs.existsSync(path.join(root, 'escaped.txt')), false);
});

test('an absolute entry name is refused', async () => {
  assert.equal((await attempt([{ name: '/etc/passwd', data: 'x' }])).code, 'unsafePath');
  assert.equal((await attempt([{ name: 'C:\\Windows\\evil.dll', data: 'x' }])).code, 'unsafePath');
});

test('a symlink entry is refused outright', async () => {
  // This is GHSA-jmr9-qjv8-65gv, the advisory against every published version
  // of extract-zip: a symlink entry pointing out of the destination, followed
  // by a second entry writing through it.
  const { code, dest } = await attempt([
    { name: 'link', data: '../../escaped', unixMode: 0xa1ff },
    { name: 'link/payload.dll', data: 'pwned' }
  ]);
  assert.equal(code, 'zipEntryUnsafe');
  assert.equal(fs.existsSync(dest), false);
});

test('a regular file with a Unix mode is still accepted', async () => {
  // Refusing symlinks must not mean refusing every archive built on Linux.
  const { code, dest } = await attempt([{ name: 'tool.sh', data: '#!/bin/sh\n', unixMode: 0o100755 }]);
  assert.equal(code, 'ok');
  assert.equal(fs.readFileSync(path.join(dest, 'tool.sh'), 'utf8'), '#!/bin/sh\n');
});

test('a DOS device name is refused', async () => {
  assert.equal((await attempt([{ name: 'CON', data: 'x' }])).code, 'reservedName');
  assert.equal((await attempt([{ name: 'sub/LPT1.txt', data: 'x' }])).code, 'reservedName');
});

test('a wrong CRC-32 is caught and the partial file removed', async () => {
  const { code, dest } = await attempt([
    { name: 'corrupt.dll', data: Buffer.alloc(2048, 0x43), crcOverride: 0xdeadbeef }
  ]);
  assert.equal(code, 'zipChecksum');
  assert.equal(fs.existsSync(path.join(dest, 'corrupt.dll')), false);
});

test('a declared length that does not match the data is caught', async () => {
  const { code } = await attempt([
    { name: 'lying.dll', data: Buffer.alloc(64, 0x44), uncompressedSizeOverride: 999_999 }
  ]);
  assert.equal(code, 'zipChecksum');
});

test('an unsupported compression method is refused', async () => {
  // 14 is LZMA: legitimate ZIP, but not something we will decode.
  assert.equal((await attempt([{ name: 'x.bin', data: 'abc', method: 14 }])).code, 'zipUnsupported');
});

test('an encrypted entry is refused', async () => {
  // Flag bit 0 is set by the builder only through a raw method; emulate the
  // encrypted case by asserting the reader surfaces it rather than guessing.
  const archive = path.join(root, 'enc.zip');
  const bytes = buildZip([{ name: 'secret.bin', data: 'abc' }]);
  // Set the general-purpose flag bit 0 in both the local and central records.
  bytes.writeUInt16LE(0x0001, 6);
  const centralOffset = bytes.readUInt32LE(bytes.length - 6);
  bytes.writeUInt16LE(0x0001, centralOffset + 8);
  fs.writeFileSync(archive, bytes);
  await assert.rejects(
    () => extractZip(archive, path.join(root, 'enc')),
    (cause: unknown) => cause instanceof AppError && cause.code === 'zipUnsupported'
  );
});

test('limits refuse an archive that would expand too far', async () => {
  const big = { name: 'big.bin', data: Buffer.alloc(64 * 1024, 0x45) };
  const { code } = await attempt([big], { ...DEFAULT_LIMITS, maxEntryBytes: 1024 });
  assert.equal(code, 'zipTooLarge');

  const { code: totalCode } = await attempt([big], { ...DEFAULT_LIMITS, maxTotalBytes: 1024 });
  assert.equal(totalCode, 'zipTooLarge');

  const { code: countCode } = await attempt(
    [{ name: 'a.txt', data: 'a' }, { name: 'b.txt', data: 'b' }],
    { ...DEFAULT_LIMITS, maxEntries: 1 }
  );
  assert.equal(countCode, 'zipTooLarge');
});

test('a file that is not a ZIP is reported as invalid', async () => {
  const archive = path.join(root, 'garbage.zip');
  fs.writeFileSync(archive, Buffer.alloc(4096, 0x7a));
  await assert.rejects(
    () => extractZip(archive, path.join(root, 'garbage')),
    (cause: unknown) => cause instanceof AppError && cause.code === 'zipInvalid'
  );
});

test('a truncated archive is reported rather than partially trusted', async () => {
  const archive = path.join(root, 'cut.zip');
  const full = buildZip([{ name: 'a.dll', data: Buffer.alloc(8192, 0x46) }]);
  fs.writeFileSync(archive, full.subarray(0, full.length - 40));
  await assert.rejects(
    () => extractZip(archive, path.join(root, 'cut')),
    (cause: unknown) => cause instanceof AppError && cause.code === 'zipInvalid'
  );
});
