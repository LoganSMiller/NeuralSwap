import type fs from 'node:fs';
import { fail } from '../../shared/errors.ts';

const EOCD_SIG = 0x06054b50;
const EOCD64_SIG = 0x06064b50;
const EOCD64_LOCATOR_SIG = 0x07064b50;
const CENTRAL_SIG = 0x02014b50;
const LOCAL_SIG = 0x04034b50;

const EOCD_MIN = 22;
const MAX_COMMENT = 0xffff;

/** Unix file-type mask and the symlink type, as stored in st_mode. */
const S_IFMT = 0xf000;
const S_IFLNK = 0xa000;

export interface ZipEntry {
  name: string;
  method: number;
  crc32: number;
  compressedSize: number;
  uncompressedSize: number;
  localHeaderOffset: number;
  /** Unix st_mode when the archive records one, else null. */
  unixMode: number | null;
  isDirectory: boolean;
  isSymlink: boolean;
  /** Bit 0 of the general-purpose flags: the entry is encrypted. */
  encrypted: boolean;
}

async function readChunk(handle: fs.promises.FileHandle, length: number, position: number): Promise<Buffer> {
  if (length <= 0) return Buffer.alloc(0);
  const buffer = Buffer.alloc(length);
  const { bytesRead } = await handle.read(buffer, 0, length, position);
  return bytesRead === length ? buffer : buffer.subarray(0, bytesRead);
}

/**
 * Find the End Of Central Directory record by scanning backwards. Its position
 * is not fixed because a ZIP may carry a trailing comment of up to 64 KiB.
 */
async function findEocd(handle: fs.promises.FileHandle, size: number): Promise<{ buffer: Buffer; offset: number }> {
  const window = Math.min(size, EOCD_MIN + MAX_COMMENT);
  const start = size - window;
  const tail = await readChunk(handle, window, start);
  for (let i = tail.length - EOCD_MIN; i >= 0; i -= 1) {
    if (tail.readUInt32LE(i) !== EOCD_SIG) continue;
    const commentLength = tail.readUInt16LE(i + 20);
    // The comment length must account for exactly the remaining bytes, which
    // is what tells a real EOCD apart from those four bytes appearing in data.
    if (i + EOCD_MIN + commentLength === tail.length) {
      return { buffer: tail.subarray(i), offset: start + i };
    }
  }
  return fail('zipInvalid', 'no end-of-central-directory record');
}

interface Directory {
  entryCount: number;
  centralOffset: number;
  centralSize: number;
}

async function readDirectoryLocation(handle: fs.promises.FileHandle, size: number): Promise<Directory> {
  const { buffer: eocd, offset: eocdOffset } = await findEocd(handle, size);
  let entryCount = eocd.readUInt16LE(10);
  let centralSize = eocd.readUInt32LE(12);
  let centralOffset = eocd.readUInt32LE(16);

  // 0xffff / 0xffffffff are the Zip64 sentinels: the real values live in the
  // Zip64 record, located through a 20-byte locator just before the EOCD.
  const needsZip64 =
    entryCount === 0xffff || centralSize === 0xffffffff || centralOffset === 0xffffffff;
  if (needsZip64 && eocdOffset >= 20) {
    const locator = await readChunk(handle, 20, eocdOffset - 20);
    if (locator.length === 20 && locator.readUInt32LE(0) === EOCD64_LOCATOR_SIG) {
      const zip64Offset = Number(locator.readBigUInt64LE(8));
      const zip64 = await readChunk(handle, 56, zip64Offset);
      if (zip64.length >= 56 && zip64.readUInt32LE(0) === EOCD64_SIG) {
        entryCount = Number(zip64.readBigUInt64LE(32));
        centralSize = Number(zip64.readBigUInt64LE(40));
        centralOffset = Number(zip64.readBigUInt64LE(48));
      }
    }
  }

  if (centralOffset < 0 || centralOffset + centralSize > size) {
    return fail('zipInvalid', 'central directory lies outside the file');
  }
  return { entryCount, centralOffset, centralSize };
}

/**
 * Zip64 stores oversized fields in extra-field block 0x0001, present only for
 * the values that actually overflowed - so the block is read positionally in
 * the same order the 32-bit fields appear.
 */
function applyZip64Extra(entry: ZipEntry, extra: Buffer): void {
  let offset = 0;
  while (offset + 4 <= extra.length) {
    const id = extra.readUInt16LE(offset);
    const size = extra.readUInt16LE(offset + 2);
    const body = extra.subarray(offset + 4, offset + 4 + size);
    if (id === 0x0001) {
      let cursor = 0;
      const next = (): number | null => {
        if (cursor + 8 > body.length) return null;
        const value = Number(body.readBigUInt64LE(cursor));
        cursor += 8;
        return value;
      };
      if (entry.uncompressedSize === 0xffffffff) entry.uncompressedSize = next() ?? entry.uncompressedSize;
      if (entry.compressedSize === 0xffffffff) entry.compressedSize = next() ?? entry.compressedSize;
      if (entry.localHeaderOffset === 0xffffffff) entry.localHeaderOffset = next() ?? entry.localHeaderOffset;
      return;
    }
    offset += 4 + size;
  }
}

/** Every entry described by the central directory, which is authoritative. */
export async function readEntries(handle: fs.promises.FileHandle, size: number): Promise<ZipEntry[]> {
  const { entryCount, centralOffset, centralSize } = await readDirectoryLocation(handle, size);
  const central = await readChunk(handle, centralSize, centralOffset);
  const entries: ZipEntry[] = [];
  let offset = 0;

  while (entries.length < entryCount) {
    if (offset + 46 > central.length) break;
    if (central.readUInt32LE(offset) !== CENTRAL_SIG) {
      return fail('zipInvalid', 'bad central directory header', { at: offset });
    }
    const versionMadeBy = central.readUInt16LE(offset + 4);
    const flags = central.readUInt16LE(offset + 8);
    const nameLength = central.readUInt16LE(offset + 28);
    const extraLength = central.readUInt16LE(offset + 30);
    const commentLength = central.readUInt16LE(offset + 32);
    const externalAttributes = central.readUInt32LE(offset + 38);

    const nameStart = offset + 46;
    const nameEnd = nameStart + nameLength;
    if (nameEnd > central.length) return fail('zipInvalid', 'truncated entry name');
    // Bit 11 marks the name as UTF-8. Older archivers use the local codepage;
    // decoding those as UTF-8 is the common convention and is lossless for the
    // ASCII paths every archive we consume actually uses.
    const name = central.subarray(nameStart, nameEnd).toString('utf8');

    // Only the Unix host (3) puts st_mode in the high half of the attributes.
    const unixMode = (versionMadeBy >> 8) === 3 ? (externalAttributes >>> 16) & 0xffff : null;

    const entry: ZipEntry = {
      name,
      method: central.readUInt16LE(offset + 10),
      crc32: central.readUInt32LE(offset + 16),
      compressedSize: central.readUInt32LE(offset + 20),
      uncompressedSize: central.readUInt32LE(offset + 24),
      localHeaderOffset: central.readUInt32LE(offset + 42),
      unixMode,
      isDirectory: name.endsWith('/') || name.endsWith('\\'),
      isSymlink: unixMode !== null && (unixMode & S_IFMT) === S_IFLNK,
      encrypted: (flags & 0x1) !== 0
    };
    applyZip64Extra(entry, central.subarray(nameEnd, nameEnd + extraLength));
    entries.push(entry);
    offset = nameEnd + extraLength + commentLength;
  }

  return entries;
}

/** Byte offset at which an entry's compressed data begins. */
export async function dataOffsetOf(handle: fs.promises.FileHandle, entry: ZipEntry): Promise<number> {
  const header = await readChunk(handle, 30, entry.localHeaderOffset);
  if (header.length < 30 || header.readUInt32LE(0) !== LOCAL_SIG) {
    return fail('zipInvalid', 'bad local file header', { name: entry.name });
  }
  const nameLength = header.readUInt16LE(26);
  const extraLength = header.readUInt16LE(28);
  return entry.localHeaderOffset + 30 + nameLength + extraLength;
}
