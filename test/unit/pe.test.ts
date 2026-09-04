import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { test } from 'node:test';
import { PeFile } from '../../src/core/pe/reader.ts';
import { buildPe, type PeSpec } from '../fixtures/pebuild.ts';
import { scratch } from '../fixtures/tmp.ts';

const root = scratch('pe');
let counter = 0;

function write(spec: PeSpec): string {
  const file = path.join(root, `image${counter++}.exe`);
  fs.writeFileSync(file, buildPe(spec));
  return file;
}

test('architecture comes from the optional-header magic', () => {
  const wide = PeFile.open(write({ bitness: 64 }));
  const narrow = PeFile.open(write({ bitness: 32 }));
  assert.ok(wide && narrow);
  assert.equal(wide.bitness, 64);
  assert.equal(narrow.bitness, 32);
  assert.equal(wide.machine, 0x8664);
  assert.equal(narrow.machine, 0x014c);
  wide.close();
  narrow.close();
});

test('the import table is read, lower-cased', () => {
  const file = write({ bitness: 64, imports: ['D3D12.dll', 'KERNEL32.dll', 'dxgi.dll'] });
  const names = PeFile.with(file, (pe) => pe.imports(), []);
  assert.deepEqual(names.sort(), ['d3d12.dll', 'dxgi.dll', 'kernel32.dll']);
});

test('delay-loaded imports are included', () => {
  // A DirectX 12 title that delay-binds d3d12 looks like it uses no graphics
  // API at all if only the normal import table is consulted.
  const file = write({
    bitness: 64,
    imports: ['KERNEL32.dll'],
    delayImports: ['d3d12.dll']
  });
  const names = PeFile.with(file, (pe) => pe.imports(), []);
  assert.ok(names.includes('d3d12.dll'), `expected d3d12.dll in ${names.join(', ')}`);
  assert.ok(names.includes('kernel32.dll'));
});

test('an image with no imports reports none rather than failing', () => {
  const names = PeFile.with(write({ bitness: 64 }), (pe) => pe.imports(), ['sentinel']);
  assert.deepEqual(names, []);
});

test('markers are found in mapped sections, across chunk boundaries', () => {
  // Place a marker deep inside a section larger than the 1 MiB scan chunk, and
  // another straddling the boundary, to prove the overlap carry works.
  const body = Buffer.alloc(3 * 1024 * 1024, 0x20);
  body.write('D3D12CreateDevice', 2_500_000, 'latin1');
  // 1 MiB boundary is at 1048576; start the string 4 bytes before it.
  body.write('vkCreateInstance', 1_048_572, 'latin1');

  const file = write({ bitness: 64, sections: [{ name: '.text', data: body }] });
  const found = PeFile.with(
    file,
    (pe) => pe.findMarkers(['D3D12CreateDevice', 'vkCreateInstance', 'Direct3DCreate9']),
    new Set<string>()
  );
  assert.equal(found.has('D3D12CreateDevice'), true);
  assert.equal(found.has('vkCreateInstance'), true);
  assert.equal(found.has('Direct3DCreate9'), false);
});

test('appended overlay data is not searched', () => {
  // This is the performance change worth pinning down. Self-extracting
  // installers and engines that bolt assets onto the executable can append
  // hundreds of megabytes after the last section. The loader can never
  // resolve a marker from there, so reading it is pure cost - and on a large
  // library it dominates scan time.
  const overlay = Buffer.alloc(64 * 1024, 0x20);
  overlay.write('D3D12CreateDevice', 1024, 'latin1');

  const file = write({
    bitness: 64,
    sections: [{ name: '.text', data: Buffer.alloc(512, 0x90) }],
    overlay
  });
  const found = PeFile.with(file, (pe) => pe.findMarkers(['D3D12CreateDevice']), new Set<string>());
  assert.equal(found.has('D3D12CreateDevice'), false);
  // And containsBytes agrees, so the two paths cannot drift apart.
  assert.equal(PeFile.with(file, (pe) => pe.containsBytes('D3D12CreateDevice'), true), false);
});

test('containsBytes finds a payload string inside a section', () => {
  const body = Buffer.alloc(4096, 0x20);
  body.write('Searching for add-ons', 100, 'latin1');
  const file = write({ bitness: 64, sections: [{ name: '.text', data: body }] });
  assert.equal(PeFile.with(file, (pe) => pe.containsBytes('Searching for add-ons'), false), true);
  assert.equal(PeFile.with(file, (pe) => pe.containsBytes('not in here at all'), true), false);
});

test('a file that is not a PE image is refused, not guessed at', () => {
  const notPe = path.join(root, 'random.bin');
  fs.writeFileSync(notPe, Buffer.alloc(8192, 0x5a));
  assert.equal(PeFile.open(notPe), null);

  const missing = path.join(root, 'does-not-exist.exe');
  assert.equal(PeFile.open(missing), null);

  // An MZ stub with no PE signature is a DOS binary, not a Windows image.
  assert.equal(PeFile.open(write({ bitness: 32, brokenSignature: true })), null);
});

test('with() closes the handle even when the callback throws', () => {
  const file = write({ bitness: 64, imports: ['kernel32.dll'] });
  assert.throws(() =>
    PeFile.with(
      file,
      () => {
        throw new Error('callback failed');
      },
      null
    )
  );
  // If the descriptor leaked, deleting the file on Windows would fail.
  fs.unlinkSync(file);
});

test('an image with no version resource reports null, and repeats cheaply', () => {
  const file = write({ bitness: 64, imports: ['kernel32.dll'] });
  PeFile.with(
    file,
    (pe) => {
      assert.equal(pe.fileVersion(), null);
      // Memoised, including the negative answer.
      assert.equal(pe.fileVersion(), null);
      assert.equal(pe.versionMentions('ReShade'), false);
      return null;
    },
    null
  );
});

test('a real system binary parses: imports, architecture and version', (t) => {
  // The synthetic fixtures prove the parser follows the spec as written; a
  // real Microsoft binary proves the spec as shipped. Skip where absent.
  const system = process.env['SystemRoot'] ?? 'C:\\Windows';
  const file = path.join(system, 'System32', 'kernel32.dll');
  if (!fs.existsSync(file)) {
    t.skip('no system binary available');
    return;
  }

  const facts = PeFile.with(
    file,
    (pe) => ({
      bitness: pe.bitness,
      imports: pe.imports(),
      version: pe.fileVersion(),
      mentionsMicrosoft: pe.versionMentions('Microsoft')
    }),
    null
  );
  assert.ok(facts, 'expected to parse kernel32.dll');
  assert.equal(facts.bitness, 64);
  assert.ok(facts.imports.length > 0, 'kernel32 should import something');
  assert.ok(
    facts.imports.some((name) => name.endsWith('.dll')),
    `expected DLL names, got ${facts.imports.slice(0, 5).join(', ')}`
  );
  assert.match(facts.version ?? '', /^\d+\.\d+\.\d+\.\d+$/);
  assert.equal(facts.mentionsMicrosoft, true);
});
