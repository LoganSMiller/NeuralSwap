import { Transform, type TransformCallback } from 'node:stream';

const TABLE = (() => {
  const table = new Int32Array(256);
  for (let i = 0; i < 256; i += 1) {
    let c = i;
    for (let bit = 0; bit < 8; bit += 1) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    table[i] = c;
  }
  return table;
})();

export function crc32(data: Buffer, seed = 0): number {
  let c = ~seed;
  for (let i = 0; i < data.length; i += 1) {
    c = (TABLE[(c ^ (data[i] as number)) & 0xff] as number) ^ (c >>> 8);
  }
  return ~c >>> 0;
}

/**
 * Passes bytes through untouched while accumulating their CRC-32 and length.
 * A ZIP entry declares both in its central directory record; checking them is
 * how we notice a truncated download or a tampered archive mid-stream rather
 * than after writing a corrupt DLL into somebody's game folder.
 */
export class Crc32Meter extends Transform {
  #crc = 0;
  #bytes = 0;

  get value(): number {
    return this.#crc;
  }

  get bytes(): number {
    return this.#bytes;
  }

  override _transform(chunk: Buffer, _encoding: BufferEncoding, done: TransformCallback): void {
    this.#crc = crc32(chunk, this.#crc);
    this.#bytes += chunk.length;
    done(null, chunk);
  }
}
