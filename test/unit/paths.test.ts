import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { test } from 'node:test';
import { assertSafeRelative, isInside, safePath } from '../../src/core/fsx/paths.ts';
import { AppError } from '../../src/shared/errors.ts';
import { scratch } from '../fixtures/tmp.ts';

const root = scratch('paths');

const codeOf = (fn: () => unknown): string => {
  try {
    fn();
  } catch (cause) {
    return cause instanceof AppError ? cause.code : `unexpected:${String(cause)}`;
  }
  return 'no-throw';
};

test('ordinary relative paths resolve under the root', () => {
  assert.equal(assertSafeRelative('bin/game.exe', root), path.join(root, 'bin', 'game.exe'));
  assert.equal(assertSafeRelative('game.exe', root), path.join(root, 'game.exe'));
  // Backslash separators are how every manifest on Windows records a path.
  assert.equal(assertSafeRelative('bin\\x64\\game.exe', root), path.join(root, 'bin', 'x64', 'game.exe'));
});

test('traversal is refused in every form', () => {
  assert.equal(codeOf(() => assertSafeRelative('../escape.txt', root)), 'unsafePath');
  assert.equal(codeOf(() => assertSafeRelative('bin/../../escape.txt', root)), 'unsafePath');
  assert.equal(codeOf(() => assertSafeRelative('bin\\..\\..\\escape.txt', root)), 'unsafePath');
  // A `..` buried mid-path still nets out inside the root, but the segment is
  // refused anyway: accepting it means trusting our own normalisation to agree
  // with Win32's, and that is the assumption these bugs are made of.
  assert.equal(codeOf(() => assertSafeRelative('bin/../game.exe', root)), 'unsafePath');
});

test('absolute and drive-relative paths are refused', () => {
  assert.equal(codeOf(() => assertSafeRelative('/etc/passwd', root)), 'unsafePath');
  assert.equal(codeOf(() => assertSafeRelative('C:\\Windows\\System32\\evil.dll', root)), 'unsafePath');
  assert.equal(codeOf(() => assertSafeRelative('\\\\server\\share\\evil.dll', root)), 'unsafePath');
});

test('alternate data streams and NUL bytes are refused', () => {
  assert.equal(codeOf(() => assertSafeRelative('game.exe:hidden', root)), 'unsafePath');
  assert.equal(codeOf(() => assertSafeRelative('a\0b', root)), 'unsafePath');
});

test('DOS device names are refused with or without an extension', () => {
  for (const name of ['CON', 'con', 'NUL', 'aux', 'COM1', 'LPT9', 'nul.txt', 'bin/CON']) {
    assert.equal(codeOf(() => assertSafeRelative(name, root)), 'reservedName', name);
  }
  // These merely start with a reserved stem and are perfectly ordinary files.
  for (const name of ['console.dll', 'nullify.txt', 'com.exe', 'lpt.dll']) {
    assert.notEqual(codeOf(() => assertSafeRelative(name, root)), 'reservedName', name);
  }
});

test('trailing dots and spaces are refused as ambiguous', () => {
  // Win32 opens `evil.` as `evil`, so validating one string and writing the
  // other is a real mismatch rather than a theoretical one.
  assert.equal(codeOf(() => assertSafeRelative('evil.', root)), 'unsafePath');
  assert.equal(codeOf(() => assertSafeRelative('evil ', root)), 'unsafePath');
  assert.equal(codeOf(() => assertSafeRelative('sub./file.txt', root)), 'unsafePath');
});

test('empty and non-string input is refused', () => {
  assert.equal(codeOf(() => assertSafeRelative('', root)), 'unsafePath');
  assert.equal(codeOf(() => assertSafeRelative(null, root)), 'unsafePath');
  assert.equal(codeOf(() => assertSafeRelative(42, root)), 'unsafePath');
  assert.equal(codeOf(() => assertSafeRelative('.', root)), 'outsideRoot');
});

test('safePath refuses a path that crosses a symlink', (t) => {
  const real = path.join(root, 'real');
  const outside = path.join(root, 'outside');
  fs.mkdirSync(real, { recursive: true });
  fs.mkdirSync(outside, { recursive: true });

  // Creating a symlink needs Developer Mode or elevation on Windows. Where it
  // is unavailable the guarantee is untestable rather than untrue, so skip.
  try {
    fs.symlinkSync(outside, path.join(real, 'link'), 'junction');
  } catch {
    t.skip('symlink creation not permitted in this environment');
    return;
  }

  assert.equal(codeOf(() => safePath(real, 'link/payload.dll')), 'symlinkRefused');
  assert.equal(codeOf(() => safePath(real, 'link')), 'symlinkRefused');
  // A sibling that is not a link is still fine.
  assert.equal(safePath(real, 'plain/payload.dll'), path.join(real, 'plain', 'payload.dll'));
});

test('isInside recognises nesting and rejects siblings', () => {
  assert.equal(isInside(path.join(root, 'a', 'b'), path.join(root, 'a')), true);
  assert.equal(isInside(path.join(root, 'a'), path.join(root, 'a')), true);
  assert.equal(isInside(path.join(root, 'ab'), path.join(root, 'a')), false);
  assert.equal(isInside(path.join(root, 'a'), path.join(root, 'a', 'b')), false);
});
