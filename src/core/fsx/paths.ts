import fs from 'node:fs';
import path from 'node:path';
import { AppError, errnoOf, fail } from '../../shared/errors.ts';

/**
 * Resolving a relative path against a game folder is the single most dangerous
 * operation in this application: every install, backup and restore funnels
 * through it, and the relative parts come from on-disk manifests and archive
 * entries rather than from us.
 *
 * A lexical prefix test is not sufficient on Windows. Each of these is a real
 * way to escape a folder that passes `dest.startsWith(root)`:
 *
 *   - `..` segments                     -> classic traversal
 *   - `file.txt:evil`                   -> NTFS alternate data stream
 *   - `sub` where sub is a junction     -> reparse point out of the tree
 *   - `CON`, `NUL`, `COM1`, `LPT1.txt`  -> DOS device, writes to a device
 *   - `sub.` / `sub ` (trailing . or ?) -> Win32 strips it, so the path we
 *                                          validated is not the path we open
 *   - a NUL byte                        -> truncates the path in Win32 APIs
 */

// CON, PRN, AUX, NUL, COM0-9, LPT0-9 - reserved whether or not they carry an
// extension, and reserved case-insensitively.
const RESERVED = /^(con|prn|aux|nul|com[0-9]|lpt[0-9])(\..*)?$/i;

/** Forward slash and backslash both separate segments on Windows. */
const SEPARATOR = new Set(['/', '\\']);

/**
 * Split on either separator without a regex. An earlier version used a
 * character class here and a lost escape silently reduced it to forward
 * slashes only - which let every backslash-separated `..` skip the segment
 * checks below. Comparing characters cannot be defeated by an escape.
 */
function splitSegments(rel: string): string[] {
  const out: string[] = [];
  let current = '';
  for (const character of rel) {
    if (SEPARATOR.has(character)) {
      out.push(current);
      current = '';
    } else {
      current += character;
    }
  }
  out.push(current);
  return out;
}

function assertUsableSegment(segment: string, rel: string): void {
  if (segment === '' || segment === '.') return;
  if (segment === '..') fail('unsafePath', 'path escapes the root', { rel });
  if (RESERVED.test(segment)) fail('reservedName', 'DOS device name', { rel, segment });
  // Win32 silently drops trailing dots and spaces, so `evil. ` and `evil` are
  // the same file to the OS but different strings to us. Refuse the ambiguity.
  if (/[. ]$/.test(segment)) fail('unsafePath', 'trailing dot or space', { rel, segment });
}

/** Validate a relative path without touching the filesystem. */
export function assertSafeRelative(rel: unknown, root: string): string {
  if (typeof rel !== 'string' || rel.length === 0) {
    fail('unsafePath', 'relative path must be a non-empty string', { rel });
  }
  if (rel.includes('\0')) fail('unsafePath', 'NUL byte in path', {});
  // A drive-relative or rooted path is never valid here, and a colon anywhere
  // else is an alternate data stream.
  if (rel.includes(':')) fail('unsafePath', 'colon in relative path', { rel });
  if (path.isAbsolute(rel)) fail('unsafePath', 'absolute path', { rel });

  for (const segment of splitSegments(rel)) assertUsableSegment(segment, rel);

  const resolvedRoot = path.resolve(root);
  const dest = path.resolve(resolvedRoot, rel);
  const back = path.relative(resolvedRoot, dest);
  if (back === '' || back === '..' || back.startsWith(`..${path.sep}`) || path.isAbsolute(back)) {
    fail('outsideRoot', 'resolved path is not inside the root', { rel, root: resolvedRoot });
  }
  return dest;
}

const sameFile = (a: string, b: string): boolean =>
  path.resolve(a).toLowerCase() === path.resolve(b).toLowerCase();

/**
 * Validate a relative path *and* prove that no existing component between the
 * root and the target is a symlink or junction. Node reports both as
 * `isSymbolicLink()` on Windows, which is exactly the set we must refuse.
 *
 * Components that do not exist yet are fine - they cannot redirect a write -
 * but we still walk past them to reach the ones that do.
 */
export function safePath(root: string, rel: string): string {
  const dest = assertSafeRelative(rel, root);
  const resolvedRoot = path.resolve(root);

  for (let item = dest; ; item = path.dirname(item)) {
    try {
      if (fs.lstatSync(item).isSymbolicLink()) {
        fail('symlinkRefused', 'path crosses a symlink or junction', { item });
      }
    } catch (cause) {
      if (errnoOf(cause) !== 'ENOENT') {
        if (cause instanceof AppError) throw cause;
        fail('unsafePath', 'could not inspect path component', { item });
      }
    }
    if (sameFile(item, resolvedRoot)) break;
    const parent = path.dirname(item);
    // Defensive: assertSafeRelative already proved dest is under root, so this
    // cannot loop forever, but a filesystem root has itself as its parent.
    if (parent === item) fail('outsideRoot', 'walked past the filesystem root', { dest });
  }
  return dest;
}

/** True when `child` is the same folder as, or nested inside, `parent`. */
export function isInside(child: string, parent: string): boolean {
  const rel = path.relative(path.resolve(parent), path.resolve(child));
  return rel === '' || (!rel.startsWith('..') && !path.isAbsolute(rel));
}
