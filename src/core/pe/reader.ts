import fs from 'node:fs';

/**
 * A read-only PE inspector: imports, architecture, version resource, and a
 * bounded search for entry-point strings.
 *
 * Why this is a class rather than the five standalone functions it replaces:
 * identifying one game executable needs its architecture, its import table,
 * its version resource and sometimes a string scan. Implemented as separate
 * functions, each one opens the file, re-reads the DOS and COFF headers and
 * re-walks the section table, then closes it - so a single candidate costs
 * four or five opens and four or five header parses. Scanning a library means
 * doing that for every executable in every folder.
 *
 * Here the file is opened once, the headers are parsed once, and each answer
 * is memoised. Nothing is executed and nothing is loaded as a module: this
 * only ever reads bytes.
 */

const DOS_MAGIC = 0x5a4d; // 'MZ'
const PE_MAGIC = 0x00004550; // 'PE\0\0'
const PE32 = 0x10b;
const PE32PLUS = 0x20b;

const DIR_IMPORT = 1;
const DIR_RESOURCE = 2;
const DIR_DELAY_IMPORT = 13;
const RT_VERSION = 16;

interface Section {
  name: string;
  virtualSize: number;
  virtualAddress: number;
  rawSize: number;
  rawOffset: number;
}

interface Headers {
  is64: boolean;
  machine: number;
  directories: { rva: number; size: number }[];
  sections: Section[];
}

export class PeFile {
  readonly path: string;
  readonly #fd: number;
  readonly #size: number;
  readonly #headers: Headers;

  #imports: string[] | undefined;
  #versionBlob: Buffer | null | undefined;
  // Bytes actually pulled off the file. Timing a parser is at the mercy of the
  // OS file cache; bytes read is deterministic, which makes it the honest
  // measure of how much IO a scan strategy costs.
  #bytesRead = 0;

  private constructor(file: string, fd: number, size: number, headers: Headers) {
    this.path = file;
    this.#fd = fd;
    this.#size = size;
    this.#headers = headers;
  }

  /** Returns null for anything that is not a readable PE image. */
  static open(file: string): PeFile | null {
    let fd: number | undefined;
    try {
      fd = fs.openSync(file, 'r');
      const size = fs.fstatSync(fd).size;
      const headers = readHeaders(fd);
      if (!headers) {
        fs.closeSync(fd);
        return null;
      }
      return new PeFile(file, fd, size, headers);
    } catch {
      if (fd !== undefined) {
        try {
          fs.closeSync(fd);
        } catch {
          /* already gone */
        }
      }
      return null;
    }
  }

  /** Open, hand to `use`, and always close - the shape most callers want. */
  static with<T>(file: string, use: (pe: PeFile) => T, fallback: T): T {
    const pe = PeFile.open(file);
    if (!pe) return fallback;
    try {
      return use(pe);
    } finally {
      pe.close();
    }
  }

  close(): void {
    try {
      fs.closeSync(this.#fd);
    } catch {
      /* closing twice is not worth reporting */
    }
  }

  /**
   * 32 or 64, from the optional-header magic. This is authoritative and needs
   * no heuristics: PE32 is 32-bit and PE32+ is 64-bit.
   */
  get bitness(): 32 | 64 {
    return this.#headers.is64 ? 64 : 32;
  }

  get machine(): number {
    return this.#headers.machine;
  }

  /**
   * Names of the DLLs this image links against, lower-cased. Delay-loaded
   * imports are included: plenty of games bind d3d12 that way, and omitting
   * them makes a DirectX 12 title look like it uses no graphics API at all.
   */
  imports(): string[] {
    if (this.#imports) return this.#imports;
    const normal = this.#nameTable(DIR_IMPORT, 20, 12);
    const delayed = this.#nameTable(DIR_DELAY_IMPORT, 32, 4);
    this.#imports = [...new Set([...normal, ...delayed])];
    return this.#imports;
  }

  /** "310.8.0.0" style version from the resource, or null. */
  fileVersion(): string | null {
    const blob = this.#version();
    if (!blob) return null;

    // VS_FIXEDFILEINFO, found by its signature rather than a fixed offset.
    const signature = blob.indexOf(Buffer.from([0xbd, 0x04, 0xef, 0xfe]));
    if (signature !== -1 && signature + 16 <= blob.length) {
      const ms = blob.readUInt32LE(signature + 8);
      const ls = blob.readUInt32LE(signature + 12);
      const fixed = [ms >>> 16, ms & 0xffff, ls >>> 16, ls & 0xffff].join('.');
      if (fixed !== '0.0.0.0') return fixed;
    }

    // Some vendor DLLs leave the fixed fields blank but still publish a
    // version in the string table. Reporting "no version" for those is what
    // makes a perfectly identifiable runtime look unknown.
    for (const key of ['FileVersion', 'ProductVersion']) {
      const needle = Buffer.from(`${key}\0`, 'utf16le');
      const at = blob.indexOf(needle);
      if (at === -1) continue;
      const value = (at + needle.length + 3) & ~3; // 32-bit aligned
      let end = value;
      while (end + 1 < blob.length && blob.readUInt16LE(end) !== 0) end += 2;
      const raw = blob.subarray(value, end).toString('utf16le').trim();
      const match = raw.match(/\d+(?:\s*[.,]\s*\d+){1,3}/);
      if (match) return match[0].replace(/\s*[.,]\s*/g, '.');
    }
    return null;
  }

  /** True when the version resource mentions `text` (resources are UTF-16). */
  versionMentions(text: string): boolean {
    const blob = this.#version();
    return blob ? blob.includes(Buffer.from(text, 'utf16le')) : false;
  }

  /**
   * Which of `markers` appear as ASCII anywhere in the image's mapped
   * sections. A game that reaches Direct3D through LoadLibrary has no import
   * entry for it, but the DLL name and the entry point it asks for are still
   * sitting in the binary as plain strings.
   *
   * Only mapped sections are searched, not the whole file. Appended overlay
   * data - the payload of a self-extracting installer, or the assets some
   * engines bolt onto the executable - can be hundreds of megabytes and cannot
   * contain a marker that the loader will ever resolve. Reading it is pure
   * cost, and on a large library it is most of the scan time.
   */
  findMarkers(markers: readonly string[]): Set<string> {
    const found = new Set<string>();
    if (markers.length === 0) return found;
    const needles = markers.map((text) => ({ text, buffer: Buffer.from(text, 'latin1') }));
    const longest = Math.max(...needles.map((n) => n.buffer.length));

    for (const span of this.#searchSpans()) {
      this.#scan(span.offset, span.length, longest, (view) => {
        for (const needle of needles) {
          if (!found.has(needle.text) && view.includes(needle.buffer)) found.add(needle.text);
        }
        return found.size === needles.length;
      });
      if (found.size === needles.length) break;
    }
    return found;
  }

  /** True when the mapped sections contain `needle`. */
  containsBytes(needle: Buffer | string): boolean {
    const buffer = typeof needle === 'string' ? Buffer.from(needle, 'latin1') : needle;
    if (buffer.length === 0) return false;
    let hit = false;
    for (const span of this.#searchSpans()) {
      this.#scan(span.offset, span.length, buffer.length, (view) => {
        if (view.includes(buffer)) hit = true;
        return hit;
      });
      if (hit) break;
    }
    return hit;
  }

  // ---- internals ----

  /** Total bytes read from this file so far. */
  get bytesRead(): number {
    return this.#bytesRead;
  }

  #read(length: number, position: number): Buffer {
    if (length <= 0) return Buffer.alloc(0);
    const buffer = Buffer.alloc(length);
    const read = fs.readSync(this.#fd, buffer, 0, length, position);
    this.#bytesRead += Math.max(read, 0);
    return read === length ? buffer : buffer.subarray(0, read);
  }

  /**
   * Byte ranges worth searching, merged and clamped to the file. Sections are
   * emitted in file order so a marker near the front is found early.
   */
  #searchSpans(): { offset: number; length: number }[] {
    const spans = this.#headers.sections
      .filter((section) => section.rawSize > 0 && section.rawOffset > 0)
      .map((section) => ({
        offset: section.rawOffset,
        length: Math.min(section.rawSize, Math.max(this.#size - section.rawOffset, 0))
      }))
      .filter((span) => span.length > 0)
      .sort((a, b) => a.offset - b.offset);

    // A malformed or packed image can describe no usable sections at all; fall
    // back to the whole file rather than silently reporting no markers.
    if (spans.length === 0) return [{ offset: 0, length: this.#size }];

    const merged: { offset: number; length: number }[] = [];
    for (const span of spans) {
      const last = merged.at(-1);
      if (last && span.offset <= last.offset + last.length) {
        last.length = Math.max(last.length, span.offset + span.length - last.offset);
      } else {
        merged.push({ ...span });
      }
    }
    return merged;
  }

  /**
   * Walk a byte range in chunks, carrying `overlap` bytes across each boundary
   * so a marker straddling two chunks is still found. `visit` returns true to
   * stop early.
   */
  #scan(
    offset: number,
    length: number,
    overlap: number,
    visit: (view: Buffer) => boolean
  ): void {
    const chunkSize = 1024 * 1024;
    const buffer = Buffer.alloc(chunkSize + overlap);
    let position = offset;
    const end = offset + length;
    let carried = 0;

    while (position < end) {
      const want = Math.min(chunkSize, end - position);
      const read = fs.readSync(this.#fd, buffer, carried, want, position);
      if (read <= 0) return;
      this.#bytesRead += read;
      const view = buffer.subarray(0, carried + read);
      if (visit(view)) return;
      carried = Math.min(overlap, view.length);
      view.subarray(view.length - carried).copy(buffer, 0);
      position += read;
    }
  }

  #rvaToOffset(rva: number): number | null {
    for (const section of this.#headers.sections) {
      const span = Math.max(section.virtualSize, section.rawSize);
      if (rva >= section.virtualAddress && rva < section.virtualAddress + span) {
        return section.rawOffset + (rva - section.virtualAddress);
      }
    }
    return null;
  }

  #cString(position: number): string {
    const buffer = this.#read(256, position);
    const end = buffer.indexOf(0);
    return buffer.subarray(0, end === -1 ? buffer.length : end).toString('latin1');
  }

  #nameTable(directoryIndex: number, stride: number, nameField: number): string[] {
    const directory = this.#headers.directories[directoryIndex];
    if (!directory || !directory.rva) return [];
    const start = this.#rvaToOffset(directory.rva);
    if (start === null) return [];

    const table = this.#read(Math.min(directory.size || 4096, 64 * 1024), start);
    const names: string[] = [];
    for (let offset = 0; offset + stride <= table.length; offset += stride) {
      const nameRva = table.readUInt32LE(offset + nameField);
      // A zeroed descriptor terminates the table.
      if (nameRva === 0) break;
      const nameOffset = this.#rvaToOffset(nameRva);
      if (nameOffset !== null) names.push(this.#cString(nameOffset).toLowerCase());
    }
    return names;
  }

  /** The raw RT_VERSION blob, memoised including the negative result. */
  #version(): Buffer | null {
    if (this.#versionBlob !== undefined) return this.#versionBlob;
    this.#versionBlob = this.#readVersionBlob();
    return this.#versionBlob;
  }

  #readVersionBlob(): Buffer | null {
    const directory = this.#headers.directories[DIR_RESOURCE];
    if (!directory || !directory.rva) return null;
    const base = this.#rvaToOffset(directory.rva);
    if (base === null) return null;

    const entriesAt = (offset: number): { id: number; offset: number }[] => {
      const header = this.#read(16, base + offset);
      if (header.length < 16) return [];
      const count = header.readUInt16LE(12) + header.readUInt16LE(14);
      const raw = this.#read(count * 8, base + offset + 16);
      const out: { id: number; offset: number }[] = [];
      for (let i = 0; i + 8 <= raw.length; i += 8) {
        out.push({ id: raw.readUInt32LE(i), offset: raw.readUInt32LE(i + 4) });
      }
      return out;
    };

    // type (RT_VERSION) -> name -> language -> data entry. The high bit marks
    // an offset as pointing at a subdirectory rather than at data.
    const type = entriesAt(0).find(
      (entry) => (entry.id & 0x7fffffff) === RT_VERSION && (entry.offset & 0x80000000) !== 0
    );
    if (!type) return null;
    const name = entriesAt(type.offset & 0x7fffffff)[0];
    if (!name || (name.offset & 0x80000000) === 0) return null;
    const language = entriesAt(name.offset & 0x7fffffff)[0];
    if (!language) return null;

    const entry = this.#read(16, base + language.offset);
    if (entry.length < 16) return null;
    const dataOffset = this.#rvaToOffset(entry.readUInt32LE(0));
    const dataSize = entry.readUInt32LE(4);
    if (dataOffset === null || !dataSize) return null;
    return this.#read(Math.min(dataSize, 64 * 1024), dataOffset);
  }
}

function readHeaders(fd: number): Headers | null {
  const read = (length: number, position: number): Buffer => {
    const buffer = Buffer.alloc(length);
    const got = fs.readSync(fd, buffer, 0, length, position);
    return got === length ? buffer : buffer.subarray(0, got);
  };

  const dos = read(0x40, 0);
  if (dos.length < 0x40 || dos.readUInt16LE(0) !== DOS_MAGIC) return null;
  const peOffset = dos.readUInt32LE(0x3c);

  // Signature (4) plus the COFF file header (20).
  const coff = read(24, peOffset);
  if (coff.length < 24 || coff.readUInt32LE(0) !== PE_MAGIC) return null;

  const machine = coff.readUInt16LE(4);
  const sectionCount = coff.readUInt16LE(6);
  const optionalSize = coff.readUInt16LE(20);
  const optionalOffset = peOffset + 24;

  const optional = read(optionalSize, optionalOffset);
  if (optional.length < 2) return null;
  const magic = optional.readUInt16LE(0);
  if (magic !== PE32 && magic !== PE32PLUS) return null;
  const is64 = magic === PE32PLUS;

  const directories: { rva: number; size: number }[] = [];
  const directoryBase = is64 ? 112 : 96;
  for (let i = 0; i < 16; i += 1) {
    const at = directoryBase + i * 8;
    if (at + 8 > optional.length) break;
    directories.push({ rva: optional.readUInt32LE(at), size: optional.readUInt32LE(at + 4) });
  }

  // A wild section count would mean a huge pointless read; real images are
  // well under this, and PE itself caps it at 96 for images.
  if (sectionCount > 256) return null;
  const table = read(sectionCount * 40, optionalOffset + optionalSize);
  const sections: Section[] = [];
  for (let i = 0; i < sectionCount; i += 1) {
    const at = i * 40;
    if (at + 40 > table.length) break;
    sections.push({
      name: table.subarray(at, at + 8).toString('latin1').replace(/\0+$/, ''),
      virtualSize: table.readUInt32LE(at + 8),
      virtualAddress: table.readUInt32LE(at + 12),
      rawSize: table.readUInt32LE(at + 16),
      rawOffset: table.readUInt32LE(at + 20)
    });
  }

  return { is64, machine, directories, sections };
}
