import zlib from 'node:zlib';
import { crc32 } from '../../src/core/zip/crc32.ts';

/**
 * A deliberately dumb ZIP writer, used only by tests.
 *
 * It exists so the extractor can be attacked with archives no honest tool
 * would produce: a `../` entry, a symlink entry, a wrong CRC, an unknown
 * compression method, a declared size that lies. Fixture binaries checked into
 * a repository cannot be reviewed; a builder can.
 */

const LOCAL_SIG = 0x04034b50;
const CENTRAL_SIG = 0x02014b50;
const EOCD_SIG = 0x06054b50;

export interface BuildEntry {
  name: string;
  /** Omitted for directory entries. */
  data?: Buffer | string;
  /** 0 = stored, 8 = deflate. Anything else is written through verbatim. */
  method?: number;
  /** Overrides the real CRC-32, to simulate corruption. */
  crcOverride?: number;
  /** Overrides the declared uncompressed size, to simulate a lying header. */
  uncompressedSizeOverride?: number;
  /** Unix st_mode. 0xa1ff makes the entry a symlink. */
  unixMode?: number;
  directory?: boolean;
}

interface Placed {
  entry: BuildEntry;
  name: Buffer;
  method: number;
  crc: number;
  compressed: Buffer;
  uncompressedSize: number;
  offset: number;
}

export function buildZip(entries: BuildEntry[]): Buffer {
  const parts: Buffer[] = [];
  const placed: Placed[] = [];
  let offset = 0;

  for (const entry of entries) {
    const isDir = entry.directory === true;
    const rawName = isDir && !entry.name.endsWith('/') ? `${entry.name}/` : entry.name;
    const name = Buffer.from(rawName, 'utf8');
    const body = isDir
      ? Buffer.alloc(0)
      : Buffer.isBuffer(entry.data)
        ? entry.data
        : Buffer.from(entry.data ?? '', 'utf8');
    const method = entry.method ?? (isDir ? 0 : 8);
    const compressed =
      method === 8 ? zlib.deflateRawSync(body) : method === 0 ? body : body;
    const crc = entry.crcOverride ?? crc32(body);
    const uncompressedSize = entry.uncompressedSizeOverride ?? body.length;

    const local = Buffer.alloc(30);
    local.writeUInt32LE(LOCAL_SIG, 0);
    local.writeUInt16LE(20, 4);
    local.writeUInt16LE(0, 6);
    local.writeUInt16LE(method, 8);
    local.writeUInt32LE(crc, 14);
    local.writeUInt32LE(compressed.length, 18);
    local.writeUInt32LE(uncompressedSize, 22);
    local.writeUInt16LE(name.length, 26);
    local.writeUInt16LE(0, 28);

    placed.push({ entry, name, method, crc, compressed, uncompressedSize, offset });
    parts.push(local, name, compressed);
    offset += local.length + name.length + compressed.length;
  }

  const centralParts: Buffer[] = [];
  let centralSize = 0;
  for (const item of placed) {
    const unixMode = item.entry.unixMode;
    const central = Buffer.alloc(46);
    central.writeUInt32LE(CENTRAL_SIG, 0);
    // High byte 3 declares the Unix host, which is what makes the external
    // attributes carry an st_mode the reader will inspect for S_IFLNK.
    central.writeUInt16LE(unixMode === undefined ? 20 : (3 << 8) | 20, 4);
    central.writeUInt16LE(20, 6);
    central.writeUInt16LE(0, 8);
    central.writeUInt16LE(item.method, 10);
    central.writeUInt32LE(item.crc, 16);
    central.writeUInt32LE(item.compressed.length, 20);
    central.writeUInt32LE(item.uncompressedSize, 24);
    central.writeUInt16LE(item.name.length, 28);
    central.writeUInt16LE(0, 30);
    central.writeUInt16LE(0, 32);
    central.writeUInt16LE(0, 34);
    central.writeUInt16LE(0, 36);
    central.writeUInt32LE(unixMode === undefined ? 0 : (unixMode << 16) >>> 0, 38);
    central.writeUInt32LE(item.offset, 42);
    centralParts.push(central, item.name);
    centralSize += central.length + item.name.length;
  }

  const eocd = Buffer.alloc(22);
  eocd.writeUInt32LE(EOCD_SIG, 0);
  eocd.writeUInt16LE(0, 4);
  eocd.writeUInt16LE(0, 6);
  eocd.writeUInt16LE(placed.length, 8);
  eocd.writeUInt16LE(placed.length, 10);
  eocd.writeUInt32LE(centralSize, 12);
  eocd.writeUInt32LE(offset, 16);
  eocd.writeUInt16LE(0, 20);

  return Buffer.concat([...parts, ...centralParts, eocd]);
}
