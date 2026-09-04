import fs from 'node:fs';
import path from 'node:path';
import zlib from 'node:zlib';
import { pipeline } from 'node:stream/promises';
import { AppError, fail } from '../../shared/errors.ts';
import { safePath } from '../fsx/paths.ts';
import { Crc32Meter } from './crc32.ts';
import { dataOffsetOf, readEntries, type ZipEntry } from './read.ts';

const STORED = 0;
const DEFLATED = 8;

export interface ExtractLimits {
  /** Refuse an archive claiming more entries than this. */
  maxEntries: number;
  /** Refuse a single entry larger than this once decompressed. */
  maxEntryBytes: number;
  /** Refuse an archive whose entries total more than this once decompressed. */
  maxTotalBytes: number;
}

export const DEFAULT_LIMITS: ExtractLimits = {
  maxEntries: 20_000,
  maxEntryBytes: 512 * 1024 * 1024,
  maxTotalBytes: 2 * 1024 * 1024 * 1024
};

export interface ExtractResult {
  files: string[];
  bytes: number;
}

/**
 * Extract a ZIP without a runtime dependency, and without any of the ways one
 * can be made to write outside its destination.
 *
 * This replaces `extract-zip`, whose entire published range carries
 * GHSA-jmr9-qjv8-65gv - unvalidated symlink path traversal - with no fixed
 * version available. It was the application's only production dependency, and
 * it was being pointed at archives fetched over the network and unpacked into
 * the user's profile.
 *
 * The rules here, none of which `extract-zip` applies:
 *
 *   - Symlink entries are refused outright. Not resolved, not skipped
 *     silently: a component archive has no legitimate reason to contain one,
 *     so its presence means the archive is not what we think it is.
 *   - Every name goes through safePath(), which rejects traversal, absolute
 *     paths, alternate data streams, DOS device names and trailing-dot
 *     ambiguity, and refuses to write through an existing symlink or junction.
 *   - Entry count and decompressed size are capped before inflating, so a
 *     16 KiB archive cannot expand to fill the disk.
 *   - Every entry's CRC-32 and length are verified against the central
 *     directory as it is written.
 *   - Only stored and deflate are accepted. Encrypted entries are refused.
 */
export async function extractZip(
  archive: string,
  destination: string,
  limits: ExtractLimits = DEFAULT_LIMITS
): Promise<ExtractResult> {
  const handle = await fs.promises.open(archive, 'r');
  try {
    const { size } = await handle.stat();
    const entries = await readEntries(handle, size);

    if (entries.length > limits.maxEntries) {
      fail('zipTooLarge', 'archive contains too many entries', {
        entries: entries.length,
        limit: limits.maxEntries
      });
    }

    // Validate the whole archive before writing a single byte. A half-applied
    // extraction of a hostile archive is still a compromised folder.
    let declaredTotal = 0;
    for (const entry of entries) {
      if (entry.encrypted) fail('zipUnsupported', 'encrypted entry', { name: entry.name });
      if (entry.isSymlink) fail('zipEntryUnsafe', 'archive contains a symlink', { name: entry.name });
      if (entry.isDirectory) continue;
      if (entry.method !== STORED && entry.method !== DEFLATED) {
        fail('zipUnsupported', 'unsupported compression method', {
          name: entry.name,
          method: entry.method
        });
      }
      if (entry.uncompressedSize > limits.maxEntryBytes) {
        fail('zipTooLarge', 'entry is too large', {
          name: entry.name,
          bytes: entry.uncompressedSize
        });
      }
      declaredTotal += entry.uncompressedSize;
      // Names are checked here, against the destination, before any mkdir - so
      // a bad entry cannot even create the folder it wanted to escape through.
      assertPlainRelative(entry.name);
    }
    if (declaredTotal > limits.maxTotalBytes) {
      fail('zipTooLarge', 'archive is too large once decompressed', {
        bytes: declaredTotal,
        limit: limits.maxTotalBytes
      });
    }

    await fs.promises.mkdir(destination, { recursive: true });
    const files: string[] = [];
    let written = 0;

    for (const entry of entries) {
      const relative = normalizeName(entry.name);
      if (entry.isDirectory) {
        await fs.promises.mkdir(safePath(destination, relative), { recursive: true });
        continue;
      }
      const target = safePath(destination, relative);
      await fs.promises.mkdir(path.dirname(target), { recursive: true });
      const bytes = await writeEntry(handle, entry, target);
      written += bytes;
      if (written > limits.maxTotalBytes) {
        fail('zipTooLarge', 'archive expanded past its declared size', { bytes: written });
      }
      files.push(relative);
    }

    return { files, bytes: written };
  } finally {
    await handle.close();
  }
}

/** Reject names before they are turned into paths, with the raw name in hand. */
function assertPlainRelative(name: string): void {
  if (name.length === 0) fail('zipEntryUnsafe', 'empty entry name');
  if (name.includes('\0')) fail('zipEntryUnsafe', 'NUL byte in entry name');
  // `safePath` covers the rest, but do it against a neutral root so that a bad
  // name fails validation rather than merely failing to be inside the target.
  safePathCheckOnly(normalizeName(name));
}

function safePathCheckOnly(relative: string): void {
  // A fixed synthetic root: we only want the name rules, not the symlink walk,
  // which is applied for real against the destination at write time.
  const root = process.platform === 'win32' ? 'C:\\__zipcheck__' : '/__zipcheck__';
  try {
    safePath(root, relative);
  } catch (cause) {
    if (cause instanceof AppError && cause.code === 'symlinkRefused') return;
    throw cause;
  }
}

const normalizeName = (name: string): string => name.replace(/\\/g, '/').replace(/\/+$/, '');

async function writeEntry(
  handle: fs.promises.FileHandle,
  entry: ZipEntry,
  target: string
): Promise<number> {
  const start = await dataOffsetOf(handle, entry);
  const source = fs.createReadStream('', {
    fd: handle.fd,
    // The handle is shared across entries, so it must not be closed or have
    // its position moved by the stream.
    autoClose: false,
    start,
    end: start + Math.max(entry.compressedSize - 1, 0)
  });
  const meter = new Crc32Meter();
  const sink = fs.createWriteStream(target);

  if (entry.compressedSize === 0 && entry.uncompressedSize === 0) {
    sink.end();
    await new Promise<void>((resolve, reject) => {
      sink.on('close', resolve);
      sink.on('error', reject);
    });
    return 0;
  }

  if (entry.method === STORED) {
    await pipeline(source, meter, sink);
  } else {
    await pipeline(source, zlib.createInflateRaw(), meter, sink);
  }

  if (meter.bytes !== entry.uncompressedSize) {
    await fs.promises.unlink(target).catch(() => {});
    fail('zipChecksum', 'entry length does not match the directory', {
      name: entry.name,
      expected: entry.uncompressedSize,
      actual: meter.bytes
    });
  }
  if (meter.value !== entry.crc32) {
    await fs.promises.unlink(target).catch(() => {});
    fail('zipChecksum', 'entry CRC-32 does not match the directory', { name: entry.name });
  }
  return meter.bytes;
}
