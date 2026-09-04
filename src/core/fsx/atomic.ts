import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { errnoOf } from '../../shared/errors.ts';

/**
 * Durable replace. `fs.writeFile` truncates the target first, so a crash or a
 * full disk part-way through leaves a half-written file where a valid one used
 * to be - which is precisely how a settings file becomes unreadable.
 *
 * Write to a sibling temp file, flush it to the platter, then rename over the
 * target. Rename is atomic on NTFS and on POSIX, so a reader sees either the
 * whole old file or the whole new one and never a torn mixture.
 */
/**
 * Windows can transiently refuse to replace a file that something else has
 * open for a moment - Defender scanning the bytes we just flushed, Search
 * indexing them, a backup agent, or Explorer previewing the folder. The
 * failure is EPERM/EACCES/EBUSY from the rename, and it clears in
 * milliseconds.
 *
 * Treating that as a hard failure is wrong: nothing is broken, and the user
 * would be told their settings could not be saved because an antivirus
 * blinked. Retry briefly, then report it honestly if it really is stuck.
 */
const TRANSIENT = new Set(['EPERM', 'EACCES', 'EBUSY', 'ENOENT']);
const RENAME_ATTEMPTS = 10;

async function renameWithRetry(from: string, to: string): Promise<void> {
  for (let attempt = 1; ; attempt += 1) {
    try {
      await fs.promises.rename(from, to);
      return;
    } catch (cause) {
      const code = errnoOf(cause);
      if (attempt >= RENAME_ATTEMPTS || code === null || !TRANSIENT.has(code)) throw cause;
      // 1, 2, 4 ... 256 ms - about half a second in total.
      await new Promise((resolve) => setTimeout(resolve, 2 ** (attempt - 1)));
    }
  }
}

export async function writeFileAtomic(file: string, data: string | Buffer): Promise<void> {
  await fs.promises.mkdir(path.dirname(file), { recursive: true });
  // A unique suffix means two writers cannot collide on the temp path. The
  // fixed `.tmp` upstream uses is safe only while writes are serialised.
  const temp = `${file}.${process.pid}.${crypto.randomBytes(4).toString('hex')}.tmp`;
  let handle: fs.promises.FileHandle | undefined;
  try {
    handle = await fs.promises.open(temp, 'wx');
    await handle.writeFile(data);
    await handle.sync();
    await handle.close();
    handle = undefined;
    await renameWithRetry(temp, file);
  } catch (cause) {
    if (handle) await handle.close().catch(() => {});
    await fs.promises.unlink(temp).catch(() => {});
    throw cause;
  }
  // Flushing the directory entry too is what makes the rename itself durable.
  // Windows does not permit opening a directory for sync; that is not an error.
  try {
    const dir = await fs.promises.open(path.dirname(file), 'r');
    await dir.sync().catch(() => {});
    await dir.close();
  } catch { /* best effort, and unavailable on Windows by design */ }
}

export const writeJsonAtomic = (file: string, value: unknown): Promise<void> =>
  writeFileAtomic(file, `${JSON.stringify(value, null, 2)}\n`);

export function readFileOrNull(file: string): string | null {
  try {
    return fs.readFileSync(file, 'utf8');
  } catch (cause) {
    if (errnoOf(cause) === 'ENOENT') return null;
    throw cause;
  }
}
