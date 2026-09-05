/**
 * Exports the behavioural guarantees of the core as language-neutral test
 * vectors: JSON case tables plus the actual binary fixtures.
 *
 * Why this exists: the largest risk in reimplementing this core in another
 * language is silently dropping an edge case. That is not hypothetical - while
 * building the TypeScript version, a lost escape in a character class reduced
 * `[\\/]` to `[\/]`, which let every backslash-separated `..` skip segment
 * validation. A hand-written test caught it in under a minute.
 *
 * A port cannot inherit those tests, but it can inherit these vectors. The
 * hostile ZIP archives and synthetic PE images are emitted as real files, so a
 * Rust or C# test suite consumes byte-identical input and asserts the same
 * error codes. "Did we preserve the rules?" becomes a test run rather than a
 * judgement call.
 *
 * The hand-written tests remain the statement of intent; these are the
 * portable record of it.
 */
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { fileURLToPath } from 'node:url';

import { assertSafeRelative } from '../src/core/fsx/paths.ts';
import { extractZip, DEFAULT_LIMITS } from '../src/core/zip/extract.ts';
import { PeFile } from '../src/core/pe/reader.ts';
import { SettingsStore } from '../src/main/state/store.ts';
import { SCHEMA_VERSION } from '../src/main/state/schema.ts';
import { buildPlan } from '../src/core/install/plan.ts';
import { decideRecovery } from '../src/core/install/recover.ts';
import { relate } from '../src/core/install/version.ts';
import { AppError, ERROR_CODES } from '../src/shared/errors.ts';
import { buildZip } from '../test/fixtures/zipbuild.ts';
import { buildPe } from '../test/fixtures/pebuild.ts';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const spec = path.join(root, 'spec');

fs.rmSync(spec, { recursive: true, force: true });
for (const dir of ['', 'zip', 'pe', 'settings', 'install']) {
  fs.mkdirSync(path.join(spec, dir), { recursive: true });
}

const writeJson = (rel, value) =>
  fs.writeFileSync(path.join(spec, rel), `${JSON.stringify(value, null, 2)}\n`);

/** Run `fn`, reporting either 'ok' or the AppError code it raised. */
function outcome(fn) {
  try {
    fn();
    return 'ok';
  } catch (error) {
    if (error instanceof AppError) return error.code;
    throw error;
  }
}

// ---------------------------------------------------------------- paths

/**
 * Every case is stated with its reason, because the reason is the part a
 * reimplementation needs in order to get the case right for the right cause.
 */
const PATH_CASES = [
  ['bin/game.exe', 'an ordinary relative path'],
  ['game.exe', 'a file at the root'],
  ['bin\\x64\\game.exe', 'backslash separators, as every Windows manifest writes them'],
  ['../escape.txt', 'classic traversal'],
  ['bin/../../escape.txt', 'traversal that nets out above the root'],
  ['bin\\..\\..\\escape.txt', 'the same attack wearing the other separator'],
  ['bin/../game.exe', 'a .. that nets out inside the root, still refused'],
  ['/etc/passwd', 'an absolute POSIX path'],
  ['C:\\Windows\\System32\\evil.dll', 'an absolute Windows path'],
  ['\\\\server\\share\\evil.dll', 'a UNC path'],
  ['game.exe:hidden', 'an NTFS alternate data stream'],
  ['a\u0000b', 'a NUL byte, which truncates the path in Win32 APIs'],
  ['evil.', 'a trailing dot, which Win32 silently strips'],
  ['evil ', 'a trailing space, which Win32 silently strips'],
  ['sub./file.txt', 'a trailing dot on an interior segment'],
  ['', 'the empty string'],
  ['.', 'the root itself, which is not a path inside the root'],
  ['CON', 'a DOS device name'],
  ['con', 'a DOS device name, lower case'],
  ['NUL', 'a DOS device name'],
  ['aux', 'a DOS device name'],
  ['COM1', 'a DOS device name'],
  ['LPT9', 'a DOS device name'],
  ['nul.txt', 'a DOS device name carrying an extension'],
  ['bin/CON', 'a DOS device name on an interior segment'],
  ['console.dll', 'merely starts with a reserved stem - a legitimate file'],
  ['nullify.txt', 'merely starts with a reserved stem - a legitimate file'],
  ['com.exe', 'merely starts with a reserved stem - a legitimate file'],
  ['lpt.dll', 'merely starts with a reserved stem - a legitimate file']
];

const PATH_ROOT = process.platform === 'win32' ? 'C:\\games\\example' : '/games/example';

writeJson('paths.json', {
  note:
    'assertSafeRelative(rel, root). "ok" means accepted; anything else is the error code that must be raised. Non-string input (null, numbers) must also be refused as unsafePath.',
  root: PATH_ROOT,
  cases: PATH_CASES.map(([rel, why]) => ({
    rel,
    why,
    expect: outcome(() => assertSafeRelative(rel, PATH_ROOT))
  }))
});

// ------------------------------------------------------------------ errors

// Every error code the UI knows how to translate.
//
// This exists because the two lists drifted: `hardwareUnsupported` was a Rust
// code with no entry in `ErrorCode`, so a user could be shown a raw machine
// string instead of a sentence. Neither side can be the authority on its own -
// one adds a code, the other never hears about it - so the set is written down
// and both are held to it.
writeJson('errors.json', {
  note:
    'Every ErrorCode, in declaration order. The Rust `Code` enum must produce exactly this set through `as_str()`, and the TypeScript `ErrorCode` union must contain exactly these members. A code in one and not the other is the bug this file exists to catch.',
  codes: ERROR_CODES
});

// ------------------------------------------------------------------ zip

const ZIP_CASES = [
  {
    name: 'benign',
    why: 'a normal archive: deflated, stored, and a directory entry',
    entries: [
      { name: 'readme.txt', data: 'hello world' },
      { name: 'bin/tool.dll', data: Buffer.alloc(4096, 0x41), method: 8 },
      { name: 'bin/raw.bin', data: Buffer.alloc(1024, 0x42), method: 0 },
      { name: 'empty/', directory: true }
    ]
  },
  {
    name: 'empty-file',
    why: 'a zero-length entry is a file, not an error',
    entries: [{ name: 'blank.txt', data: '' }]
  },
  {
    name: 'traversal-slash',
    why: 'a ../ entry must be refused before anything is written',
    entries: [
      { name: 'good.txt', data: 'fine' },
      { name: '../escaped.txt', data: 'pwned' }
    ]
  },
  {
    name: 'traversal-backslash',
    why: '7-Zip and older Windows archivers write backslash separators',
    entries: [{ name: '..\\escaped.txt', data: 'pwned' }]
  },
  {
    name: 'absolute-posix',
    why: 'an absolute entry name',
    entries: [{ name: '/etc/passwd', data: 'x' }]
  },
  {
    name: 'absolute-windows',
    why: 'an absolute Windows entry name',
    entries: [{ name: 'C:\\Windows\\evil.dll', data: 'x' }]
  },
  {
    name: 'symlink-escape',
    why: 'GHSA-jmr9-qjv8-65gv exactly: a symlink out of the tree, then a write through it',
    entries: [
      { name: 'link', data: '../../escaped', unixMode: 0xa1ff },
      { name: 'link/payload.dll', data: 'pwned' }
    ]
  },
  {
    name: 'unix-mode-regular-file',
    why: 'refusing symlinks must not mean refusing archives built on Linux',
    entries: [{ name: 'tool.sh', data: '#!/bin/sh\n', unixMode: 0o100755 }]
  },
  {
    name: 'device-name',
    why: 'a DOS device name as an entry',
    entries: [{ name: 'CON', data: 'x' }]
  },
  {
    name: 'device-name-nested',
    why: 'a DOS device name on an interior segment',
    entries: [{ name: 'sub/LPT1.txt', data: 'x' }]
  },
  {
    name: 'bad-crc',
    why: 'a CRC-32 that does not match the data',
    entries: [{ name: 'corrupt.dll', data: Buffer.alloc(2048, 0x43), crcOverride: 0xdeadbeef }]
  },
  {
    name: 'lying-length',
    why: 'a declared uncompressed size that does not match the data',
    entries: [
      { name: 'lying.dll', data: Buffer.alloc(64, 0x44), uncompressedSizeOverride: 999_999 }
    ]
  },
  {
    name: 'unsupported-method',
    why: 'method 14 is LZMA: valid ZIP, but not something we decode',
    entries: [{ name: 'x.bin', data: 'abc', method: 14 }]
  }
];

const zipRows = [];
for (const testCase of ZIP_CASES) {
  const file = `zip/${testCase.name}.zip.bin`;
  fs.writeFileSync(path.join(spec, file), buildZip(testCase.entries));
  const dest = fs.mkdtempSync(path.join(os.tmpdir(), 'ns-vec-'));
  let expect;
  try {
    await extractZip(path.join(spec, file), dest, DEFAULT_LIMITS);
    expect = 'ok';
  } catch (error) {
    if (!(error instanceof AppError)) throw error;
    expect = error.code;
  }
  fs.rmSync(dest, { recursive: true, force: true });
  zipRows.push({ file, why: testCase.why, expect });
}

// An encrypted entry needs the general-purpose flag set in both records, which
// the builder does not model, so it is patched in afterwards.
{
  const bytes = buildZip([{ name: 'secret.bin', data: 'abc' }]);
  bytes.writeUInt16LE(0x0001, 6);
  const centralOffset = bytes.readUInt32LE(bytes.length - 6);
  bytes.writeUInt16LE(0x0001, centralOffset + 8);
  const file = 'zip/encrypted.zip.bin';
  fs.writeFileSync(path.join(spec, file), bytes);
  zipRows.push({ file, why: 'an encrypted entry is refused, not silently skipped', expect: 'zipUnsupported' });
}

// Not a ZIP at all, and a ZIP with its tail cut off.
fs.writeFileSync(path.join(spec, 'zip/not-a-zip.zip.bin'), Buffer.alloc(4096, 0x7a));
zipRows.push({ file: 'zip/not-a-zip.zip.bin', why: 'a file that is not a ZIP', expect: 'zipInvalid' });
{
  const full = buildZip([{ name: 'a.dll', data: Buffer.alloc(8192, 0x46) }]);
  fs.writeFileSync(path.join(spec, 'zip/truncated.zip.bin'), full.subarray(0, full.length - 40));
  zipRows.push({ file: 'zip/truncated.zip.bin', why: 'a truncated download', expect: 'zipInvalid' });
}

writeJson('zip/cases.json', {
  note:
    'extractZip(file, freshEmptyDirectory). "ok" means every entry extracted and verified; anything else is the error code. On any failure the destination must contain no files - validation precedes writing.',
  limits: DEFAULT_LIMITS,
  cases: zipRows
});

// ------------------------------------------------------------------- pe

const PE_CASES = [
  {
    name: 'x64-d3d12',
    why: 'a 64-bit image importing d3d12',
    spec: { bitness: 64, imports: ['D3D12.dll', 'KERNEL32.dll', 'dxgi.dll'] }
  },
  {
    name: 'x86-d3d11',
    why: 'a 32-bit image, to pin the PE32 vs PE32+ distinction',
    spec: { bitness: 32, imports: ['d3d11.dll', 'KERNEL32.dll'] }
  },
  {
    name: 'x64-delay-d3d12',
    why: 'a delay-bound d3d12 - invisible if only the normal import table is read',
    spec: { bitness: 64, imports: ['KERNEL32.dll'], delayImports: ['d3d12.dll'] }
  },
  {
    name: 'no-imports',
    why: 'an image with no import directory reports none rather than failing',
    spec: { bitness: 64 }
  },
  {
    name: 'marker-in-section',
    why: 'a marker reachable only as a string, past the 1 MiB scan chunk boundary',
    spec: {
      bitness: 64,
      sections: [{ name: '.text', data: markerBody() }]
    }
  },
  {
    name: 'marker-in-overlay-only',
    why:
      'the marker sits in appended overlay data, which the loader can never resolve - it must NOT be found, and only the mapped sections may be read',
    spec: {
      bitness: 64,
      sections: [{ name: '.text', data: Buffer.alloc(512, 0x90) }],
      overlay: overlayBody()
    }
  },
  {
    name: 'dos-stub-only',
    why: 'an MZ stub with no PE signature is a DOS binary, not a Windows image',
    spec: { bitness: 32, brokenSignature: true }
  }
];

function markerBody() {
  const body = Buffer.alloc(3 * 1024 * 1024, 0x20);
  body.write('D3D12CreateDevice', 2_500_000, 'latin1');
  body.write('vkCreateInstance', 1_048_572, 'latin1');
  return body;
}

function overlayBody() {
  const overlay = Buffer.alloc(64 * 1024, 0x20);
  overlay.write('D3D12CreateDevice', 1024, 'latin1');
  return overlay;
}

const PE_MARKERS = [
  'D3D12CreateDevice',
  'D3D11CreateDevice',
  'CreateDXGIFactory',
  'Direct3DCreate9',
  'vkCreateInstance',
  'wglCreateContext'
];

const peRows = [];
for (const testCase of PE_CASES) {
  const file = `pe/${testCase.name}.pe.bin`;
  fs.writeFileSync(path.join(spec, file), buildPe(testCase.spec));
  const observed = PeFile.with(
    path.join(spec, file),
    (pe) => ({
      parses: true,
      bitness: pe.bitness,
      machine: pe.machine,
      imports: [...pe.imports()].sort(),
      markers: [...pe.findMarkers(PE_MARKERS)].sort(),
      bytesRead: pe.bytesRead
    }),
    { parses: false }
  );
  peRows.push({
    file,
    why: testCase.why,
    sizeBytes: fs.statSync(path.join(spec, file)).size,
    expect: observed
  });
}

writeJson('pe/cases.json', {
  note:
    'Open each image and report bitness, machine, imports (lower-cased, normal plus delay-load) and which markers appear in the MAPPED SECTIONS. parses:false means the file must be refused as not-a-PE. bytesRead is informational, not a required value - but for marker-in-overlay-only it must stay far below the file size.',
  markers: PE_MARKERS,
  cases: peRows
});

// -------------------------------------------------------------- settings

const SETTINGS_CASES = [
  {
    name: 'v1-upstream',
    why: 'the upstream library.json layout migrates forward rather than being discarded',
    input: {
      theme: 'dark',
      lang: 'de',
      folders: ['E:\\SteamLibrary'],
      hidden: ['E:\\SteamLibrary\\Skyrim'],
      addon: 'C:\\builds\\renodx-dlss.addon64',
      addonFiles: [{ path: 'C:\\builds\\other.addon64', name: 'Other' }],
      posters: { abc123: 'C:\\posters\\abc123.png' }
    }
  },
  {
    name: 'current',
    why: 'a file at the current schema loads unchanged',
    input: { schema: SCHEMA_VERSION, lang: 'ja', folders: ['D:\\Games'] }
  },
  {
    name: 'partly-malformed',
    why: 'one bad field costs that field, not the other forty',
    input: {
      schema: SCHEMA_VERSION,
      lang: 'es',
      folders: ['G:\\Games'],
      theme: 'chartreuse',
      groupGamesByStore: 'yes please',
      recents: 'not an array',
      scans: { abc: { ok: true, rules: 7, scannedAt: 5 }, bad: 'nope' }
    }
  },
  {
    name: 'scan-without-rules-stamp',
    why: 'a cached verdict with no detection-generation stamp is dropped so it is rescanned',
    input: { schema: SCHEMA_VERSION, scans: { old: { ok: true, api: 'dxgi' } } }
  },
  {
    name: 'from-the-future',
    why: 'settings written by a newer build are refused, and the file left untouched',
    input: { schema: SCHEMA_VERSION + 5, lang: 'it', futureField: 1 }
  },
  { name: 'not-json', why: 'a truncated write', raw: '{ this is not json' },
  { name: 'json-array', why: 'valid JSON that is not an object', raw: '[1,2,3]' }
];

const settingsRows = [];
for (const testCase of SETTINGS_CASES) {
  const file = `settings/${testCase.name}.json`;
  const raw = testCase.raw ?? `${JSON.stringify(testCase.input, null, 2)}\n`;
  fs.writeFileSync(path.join(spec, file), raw);

  // Load a throwaway copy: opening quarantines a corrupt file by renaming it,
  // and the vector must survive for the next implementation to read.
  const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'ns-vec-'));
  const copy = path.join(scratch, 'settings.json');
  fs.copyFileSync(path.join(spec, file), copy);

  let row;
  try {
    const store = SettingsStore.open(copy);
    row = { status: store.health().status, settings: store.get() };
  } catch (error) {
    if (!(error instanceof AppError)) throw error;
    row = { status: 'refused', code: error.code };
  }
  fs.rmSync(scratch, { recursive: true, force: true });
  settingsRows.push({ file, why: testCase.why, expect: row });
}

writeJson('settings/cases.json', {
  note:
    'Open each file as the settings store. status is one of fresh|loaded|migrated|recoveredFromBackup|quarantined, or "refused" with a code when the file belongs to a newer build. A file that cannot be read must be set aside, never deleted, and never silently replaced with defaults.',
  schemaVersion: SCHEMA_VERSION,
  cases: settingsRows
});

// --------------------------------------------------------- version ordering

/**
 * Version comparison decides whether an install is an upgrade or a downgrade,
 * which is the one thing a user is guaranteed to notice getting wrong. The
 * awkward cases are all real: PE string tables write `"310, 8, 0, 0"`, and
 * component counts differ between a package name and a version resource.
 *
 * The prefix case is here because the two implementations disagreed. JavaScript's
 * `parseInt('8abc')` returns 8, so `310.8abc` compared as `310.8`, while a
 * strict parse rejects it. Strict is right, and this row is what holds both to it.
 */
const VERSION_CASES = [
  ['310.8.0.0', '310.1.0.0', 'a straightforward upgrade'],
  ['310.1.0.0', '310.8.0.0', 'a straightforward downgrade'],
  ['310.8.0.0', '310.8.0.0', 'identical'],
  ['310.8', '310.8.0.0', 'trailing zeroes are not a difference'],
  ['310.8.0.0', '310.8', 'the same, stated the other way round'],
  ['310, 8, 0, 0', '310.8.0.0', 'a PE string table against a dotted version'],
  ['310.0.0.0', '9.0.0.0', 'numeric, not lexical: 310 is not less than 9'],
  ['2.13.0.0', '2.9.0.0', 'numeric again, one component down'],
  [null, '310.8.0.0', 'no package version'],
  ['310.8.0.0', null, 'no version resource on the file'],
  [null, null, 'neither side has a version'],
  ['', '310.8.0.0', 'an empty string is not a version'],
  ['not a version', '310.8.0.0', 'unparsable text'],
  ['310.8.beta', '310.8.0.0', 'a non-numeric component'],
  ['310.8abc', '310.8.0.0', 'a numeric prefix must not be accepted as the number']
];

writeJson('install/versions.json', {
  note:
    'relate(packageVersion, presentVersion) -> newer|older|same|unknown. unknown when either side has no parsable version, and the caller must treat that as "cannot claim this is an upgrade" rather than as equality. A component that is not a plain number makes the whole string unparsable - accepting a numeric prefix is a bug.',
  cases: VERSION_CASES.map(([packageVersion, presentVersion, why]) => ({
    packageVersion,
    presentVersion,
    why,
    expect: relate(packageVersion, presentVersion)
  }))
});

// ------------------------------------------------------------ install plan

/**
 * A plan is what the user agrees to before anything is written, so these cases
 * are the record of what they will be shown. Every one is a real shape seen on
 * a games drive rather than an invented edge: the mixed-version cohort and the
 * stray copy in the game root both come from an actual install on the machine
 * this was built on.
 */
const DLSS = { kind: 'dlss', version: '310.8.0.0', size: 1000, sha256: 'pkg-dlss-310800' };

const PLAN_CASES = [
  {
    name: 'fresh-install',
    why: 'nothing installed yet: create, no backup',
    input: { installDir: 'bin/x64', pkg: [{ name: 'nvngx_dlss.dll', ...DLSS }], present: [] }
  },
  {
    name: 'already-identical',
    why: 'the same bytes are already there, so the plan does nothing at all',
    input: {
      installDir: 'bin/x64',
      pkg: [{ name: 'nvngx_dlss.dll', ...DLSS }],
      present: [
        {
          rel: 'bin/x64/nvngx_dlss.dll',
          kind: 'dlss',
          version: '310.8.0.0',
          size: 1000,
          sha256: 'pkg-dlss-310800',
          managed: true
        }
      ]
    }
  },
  {
    name: 'upgrade',
    why: 'a newer package version replaces an older file and backs it up',
    input: {
      installDir: 'bin/x64',
      pkg: [{ name: 'nvngx_dlss.dll', ...DLSS }],
      present: [
        {
          rel: 'bin/x64/nvngx_dlss.dll',
          kind: 'dlss',
          version: '310.1.0.0',
          size: 900,
          sha256: 'game-dlss-310100',
          managed: false
        }
      ]
    }
  },
  {
    name: 'downgrade',
    why: 'an older package version is still allowed, but must be named a downgrade',
    input: {
      installDir: 'bin/x64',
      pkg: [{ name: 'nvngx_dlss.dll', ...DLSS }],
      present: [
        {
          rel: 'bin/x64/nvngx_dlss.dll',
          kind: 'dlss',
          version: '310.9.0.0',
          size: 1100,
          sha256: 'newer-than-package',
          managed: false
        }
      ]
    }
  },
  {
    name: 'same-version-different-bytes',
    why: 'already swapped by hand: the version matches but the file does not',
    input: {
      installDir: 'bin/x64',
      pkg: [{ name: 'nvngx_dlss.dll', ...DLSS }],
      present: [
        {
          rel: 'bin/x64/nvngx_dlss.dll',
          kind: 'dlss',
          version: '310.8.0.0',
          size: 1000,
          sha256: 'a-different-build',
          managed: false
        }
      ]
    }
  },
  {
    name: 'unknown-version-on-disk',
    why: 'a DLL with no version resource is replaced without claiming an upgrade',
    input: {
      installDir: 'bin/x64',
      pkg: [{ name: 'nvngx_dlss.dll', ...DLSS }],
      present: [
        {
          rel: 'bin/x64/nvngx_dlss.dll',
          kind: 'dlss',
          version: null,
          size: 900,
          sha256: 'unversioned',
          managed: false
        }
      ]
    }
  },
  {
    name: 'mixed-version-cohort',
    why:
      'upgrading one DLL and leaving its sibling behind at the old version - the shape that crashes a game on launch',
    input: {
      installDir: 'bin/x64',
      pkg: [{ name: 'nvngx_dlss.dll', ...DLSS }],
      present: [
        {
          rel: 'bin/x64/nvngx_dlss.dll',
          kind: 'dlss',
          version: '310.1.0.0',
          size: 900,
          sha256: 'game-dlss-310100',
          managed: false
        },
        {
          rel: 'bin/x64/nvngx_dlssg.dll',
          kind: 'dlss',
          version: '310.1.0.0',
          size: 800,
          sha256: 'game-dlssg-310100',
          managed: false
        }
      ]
    }
  },
  {
    name: 'kinds-number-independently',
    why: 'DLSS 310.8.0.0 beside Streamline 2.13.0.0 is correct and must not be flagged',
    input: {
      installDir: 'bin/x64',
      pkg: [{ name: 'nvngx_dlss.dll', ...DLSS }],
      present: [
        {
          rel: 'bin/x64/sl.dlss.dll',
          kind: 'streamline',
          version: '2.13.0.0',
          size: 500,
          sha256: 'sl-dlss-21300',
          managed: false
        }
      ]
    }
  },
  {
    name: 'stray-copy-in-game-root',
    why:
      'a hand-placed copy where the loader will never look: not the target, and not part of the cohort',
    input: {
      installDir: 'bin/x64',
      pkg: [{ name: 'nvngx_dlss.dll', ...DLSS }],
      present: [
        {
          rel: 'nvngx_dlss.dll',
          kind: 'dlss',
          version: '999.0.0.0',
          size: 1200,
          sha256: 'stray',
          managed: false
        }
      ]
    }
  },
  {
    name: 'adds-kind-not-present',
    why: 'adding frame generation to a game that never shipped it is a bigger change',
    input: {
      installDir: 'bin/x64',
      pkg: [
        { name: 'sl.dlss_g.dll', kind: 'streamline', version: '2.13.0.0', size: 700, sha256: 'slg' }
      ],
      present: [
        {
          rel: 'bin/x64/nvngx_dlss.dll',
          kind: 'dlss',
          version: '310.8.0.0',
          size: 1000,
          sha256: 'pkg-dlss-310800',
          managed: false
        }
      ]
    }
  },
  {
    name: 'install-into-game-root',
    why: 'an empty install directory means the game folder itself',
    input: {
      installDir: '',
      pkg: [{ name: 'nvngx_dlss.dll', ...DLSS }],
      present: [
        {
          rel: 'nvngx_dlss.dll',
          kind: 'dlss',
          version: '310.1.0.0',
          size: 900,
          sha256: 'root-old',
          managed: false
        }
      ]
    }
  },
  {
    name: 'case-and-separator-insensitive',
    why:
      'the scanner reports backslashes and whatever case the directory entry had; NTFS says these are one file',
    input: {
      installDir: 'Bin/X64',
      pkg: [{ name: 'nvngx_dlss.dll', ...DLSS }],
      present: [
        {
          rel: 'bin\\x64\\NVNGX_DLSS.DLL',
          kind: 'dlss',
          version: '310.1.0.0',
          size: 900,
          sha256: 'other-case',
          managed: false
        }
      ]
    }
  },
  {
    name: 'sort-order-is-stable',
    why: 'steps are ordered by path, not by package order, so two runs agree',
    input: {
      installDir: 'bin/x64',
      pkg: [
        { name: 'sl.dlss.dll', kind: 'streamline', version: '2.13.0.0', size: 3, sha256: 'c' },
        { name: 'nvngx_dlss.dll', ...DLSS },
        { name: 'sl.common.dll', kind: 'streamline', version: '2.13.0.0', size: 2, sha256: 'b' }
      ],
      present: []
    }
  },
  // Refusals. A package entry is data from an archive, so it gets the same
  // suspicion as a ZIP entry name.
  { name: 'entry-traversal', why: 'a package entry escaping the install directory', input: { installDir: 'bin/x64', pkg: [{ name: '../escape.dll', ...DLSS }], present: [] } },
  { name: 'entry-nested', why: 'a package entry with a separator is not a plain file name', input: { installDir: 'bin/x64', pkg: [{ name: 'sub/nested.dll', ...DLSS }], present: [] } },
  { name: 'entry-absolute-windows', why: 'an absolute Windows path as a package entry', input: { installDir: 'bin/x64', pkg: [{ name: 'C:\\Windows\\evil.dll', ...DLSS }], present: [] } },
  { name: 'entry-stream', why: 'an NTFS alternate data stream as a package entry', input: { installDir: 'bin/x64', pkg: [{ name: 'x.dll:hidden', ...DLSS }], present: [] } },
  { name: 'entry-device-name', why: 'a DOS device name as a package entry', input: { installDir: 'bin/x64', pkg: [{ name: 'NUL', ...DLSS }], present: [] } },
  { name: 'entry-trailing-dot', why: 'Win32 strips the trailing dot, so the validated name is not the opened name', input: { installDir: 'bin/x64', pkg: [{ name: 'evil.dll.', ...DLSS }], present: [] } },
  { name: 'entry-nul-byte', why: 'a NUL byte truncates the path in Win32 APIs', input: { installDir: 'bin/x64', pkg: [{ name: `x${String.fromCharCode(0)}.dll`, ...DLSS }], present: [] } },
  { name: 'entry-legitimate-lookalike', why: 'a name that merely contains a reserved stem is an ordinary file', input: { installDir: 'bin/x64', pkg: [{ name: 'nullify.dll', ...DLSS }], present: [] } },
  { name: 'package-empty', why: 'a package with no runtime files is not an install', input: { installDir: 'bin/x64', pkg: [], present: [] } },
  {
    name: 'package-duplicate-entry',
    why: 'two entries differing only in case are one file on NTFS',
    input: {
      installDir: 'bin/x64',
      pkg: [{ name: 'nvngx_dlss.dll', ...DLSS }, { name: 'NVNGX_DLSS.DLL', ...DLSS }],
      present: []
    }
  }
];

const planRows = PLAN_CASES.map((testCase) => {
  const request = { route: 'nativeDll', ...testCase.input };
  let expect;
  try {
    expect = buildPlan(request);
  } catch (error) {
    if (!(error instanceof AppError)) throw error;
    expect = { refused: error.code };
  }
  return { name: testCase.name, why: testCase.why, input: request, expect };
});

writeJson('install/plan.json', {
  note:
    'buildPlan(input) is pure - no filesystem access - so the dry run a user approves and the run that follows cannot disagree. Compare the whole structure: steps in order, reasons, version transitions, byte totals and warnings. A `refused` value is the error code that must be raised instead. Paths in `present` may use either separator and any case; NTFS treats them as one file and so must the plan.',
  cases: planRows
});

// -------------------------------------------------------- journal recovery

/**
 * The states an interrupted install can leave behind. Reaching these on
 * purpose means staging a crash at an exact instant, which is why the decision
 * is a pure function over what survived rather than a branch inside the
 * writer.
 */
const RECOVERY_CASES = [
  { name: 'no-plan', why: 'the journal was still being created; the plan is fsynced before any target is opened', state: { hasPlan: false, planReadable: false, committed: false, appliedSteps: 0, totalSteps: 0 } },
  { name: 'plan-unreadable', why: 'without the plan we cannot know which backup belongs to which target, so it is kept rather than guessed at', state: { hasPlan: true, planReadable: false, committed: false, appliedSteps: 2, totalSteps: 4 } },
  { name: 'committed', why: 'every step landed and verified; only the backups need clearing', state: { hasPlan: true, planReadable: true, committed: true, appliedSteps: 4, totalSteps: 4 } },
  { name: 'committed-short-progress', why: 'the commit marker is the authority on completion - a progress log missing its last fsync is harmless', state: { hasPlan: true, planReadable: true, committed: true, appliedSteps: 3, totalSteps: 4 } },
  { name: 'nothing-applied', why: 'a plan was written but no file was touched', state: { hasPlan: true, planReadable: true, committed: false, appliedSteps: 0, totalSteps: 4 } },
  { name: 'partly-applied', why: 'the half-changed folder: roll back, because that needs only the backups sitting beside the journal', state: { hasPlan: true, planReadable: true, committed: false, appliedSteps: 2, totalSteps: 4 } },
  { name: 'all-applied-not-committed', why: 'without the marker the last file is not known to be intact, so the folder is not known-good', state: { hasPlan: true, planReadable: true, committed: false, appliedSteps: 4, totalSteps: 4 } },
  { name: 'progress-exceeds-plan', why: 'one of the two files is lying and guessing which means guessing what to restore', state: { hasPlan: true, planReadable: true, committed: false, appliedSteps: 5, totalSteps: 4 } },
  { name: 'negative-counts', why: 'a crash can truncate the progress log mid-line; the parser must not talk us into restoring a negative number of files', state: { hasPlan: true, planReadable: true, committed: false, appliedSteps: -1, totalSteps: 4 } }
];

const recoveryRows = RECOVERY_CASES.map((testCase) => {
  const state = { id: testCase.name, ...testCase.state };
  return { name: testCase.name, why: testCase.why, state, expect: decideRecovery(state) };
});

writeJson('install/recovery.json', {
  note:
    'decideRecovery(state) over what survived on disk. discard removes an inert journal; rollBack restores the backups in reverse order and then removes them; finishCleanup removes the journal of a committed install but keeps its backups, which the install manifest points at so the original files can be put back later; quarantine keeps the journal untouched and tells the user. The default for a half-applied install is rollBack, not roll-forward: finishing needs the source package to still exist and still be the same package, and neither can be assumed after an unclean shutdown.',
  cases: recoveryRows
});

// -------------------------------------------------------------- summary

const counts = {
  paths: PATH_CASES.length,
  zip: zipRows.length,
  pe: peRows.length,
  settings: settingsRows.length,
  versions: VERSION_CASES.length,
  plan: planRows.length,
  recovery: recoveryRows.length
};

fs.writeFileSync(
  path.join(spec, 'README.md'),
  `# Behavioural vectors

Generated by \`node scripts/export-vectors.mjs\` from the reference
implementation. **Do not edit by hand** - regenerate.

These record what the core must do, independent of the language it is written
in. A reimplementation reads the same JSON tables and the same binary
fixtures, and must produce the same verdicts and the same error codes.

| Area | Cases | Table | Fixtures |
| --- | --- | --- | --- |
| Path validation | ${counts.paths} | \`paths.json\` | none |
| Archive extraction | ${counts.zip} | \`zip/cases.json\` | \`zip/*.zip.bin\` |
| PE inspection | ${counts.pe} | \`pe/cases.json\` | \`pe/*.pe.bin\` |
| Settings loading | ${counts.settings} | \`settings/cases.json\` | \`settings/*.json\` |
| Version ordering | ${counts.versions} | \`install/versions.json\` | none |
| Install planning | ${counts.plan} | \`install/plan.json\` | none |
| Journal recovery | ${counts.recovery} | \`install/recovery.json\` | none |

The archives under \`zip/\` are **deliberately hostile** - they contain
traversal entries, a symlink escape, bad checksums and a lying length header.
They are inert data and safe to keep in a repository, but do not extract them
with a general-purpose tool expecting nothing to happen.

Every fixture carries a \`.bin\` extension rather than \`.zip\` or \`.exe\`.
That is deliberate: a repository full of small executables containing Direct3D
entry-point strings and no valid entry point, next to archives full of
traversal entries, is an antivirus quarantine waiting to happen on every clone.
The parsers are handed bytes and do not care what the file is called.

Error codes are the stable contract; messages are not. Every \`expect\` value
other than \`ok\` is a code that must be raised for that case, and for the
right reason - the \`why\` field states the reason.

The hand-written tests under \`test/unit\` remain the statement of intent.
These vectors are the portable record of it, and exist because the largest
risk in a port is silently dropping one of these cases.
`
);

console.log(
  `exported ${Object.values(counts).reduce((a, b) => a + b, 0)} vectors to spec/ ` +
    `(paths ${counts.paths}, zip ${counts.zip}, pe ${counts.pe}, ` +
    `settings ${counts.settings}, versions ${counts.versions}, ` +
    `plan ${counts.plan}, recovery ${counts.recovery})`
);
