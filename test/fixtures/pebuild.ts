/**
 * A minimal PE image writer for tests.
 *
 * Real game executables cannot be checked into a repository, and a binary
 * fixture cannot be reviewed in a diff. This builds just enough of a valid PE
 * to exercise the reader: correct DOS and COFF headers, a PE32 or PE32+
 * optional header with data directories, a section table, an import
 * descriptor table with DLL names, arbitrary section payloads, and an optional
 * appended overlay.
 */

const FILE_ALIGNMENT = 0x200;
const SECTION_ALIGNMENT = 0x1000;

export interface SectionSpec {
  name: string;
  /** Raw bytes for the section body. */
  data: Buffer;
}

export interface PeSpec {
  bitness: 32 | 64;
  /** DLL names for the import descriptor table. */
  imports?: string[];
  /** DLL names for the delay-load import descriptor table. */
  delayImports?: string[];
  /** Extra sections beyond the generated import section. */
  sections?: SectionSpec[];
  /** Bytes appended after the last section - not part of any mapped range. */
  overlay?: Buffer;
  /** Emit a DOS stub with no PE signature, to test rejection. */
  brokenSignature?: boolean;
}

const align = (value: number, to: number): number => Math.ceil(value / to) * to;

interface Placed extends SectionSpec {
  virtualAddress: number;
  rawOffset: number;
  rawSize: number;
}

export function buildPe(spec: PeSpec): Buffer {
  const is64 = spec.bitness === 64;
  const importNames = spec.imports ?? [];
  const delayNames = spec.delayImports ?? [];

  // The import section is laid out first so its RVAs can be computed before
  // the headers that must point at them are written.
  const importSection = buildImportSection(importNames, delayNames);
  const sections: SectionSpec[] = [];
  if (importSection.data.length > 0) sections.push({ name: '.rdata', data: importSection.data });
  sections.push(...(spec.sections ?? []));
  if (sections.length === 0) sections.push({ name: '.text', data: Buffer.alloc(16) });

  const optionalSize = (is64 ? 112 : 96) + 16 * 8;
  const headerSize = 0x40 + 24 + optionalSize + sections.length * 40;
  const firstRawOffset = align(headerSize, FILE_ALIGNMENT);

  const placed: Placed[] = [];
  let rawOffset = firstRawOffset;
  let virtualAddress = SECTION_ALIGNMENT;
  for (const section of sections) {
    const rawSize = align(Math.max(section.data.length, 1), FILE_ALIGNMENT);
    placed.push({ ...section, virtualAddress, rawOffset, rawSize });
    rawOffset += rawSize;
    virtualAddress += align(Math.max(section.data.length, 1), SECTION_ALIGNMENT);
  }

  const rdata = placed.find((s) => s.name === '.rdata');
  const importRva = rdata && importSection.importTableSize > 0 ? rdata.virtualAddress + importSection.importTableOffset : 0;
  const delayRva = rdata && importSection.delayTableSize > 0 ? rdata.virtualAddress + importSection.delayTableOffset : 0;

  // ---- DOS header ----
  const dos = Buffer.alloc(0x40);
  dos.writeUInt16LE(0x5a4d, 0); // 'MZ'
  dos.writeUInt32LE(0x40, 0x3c); // e_lfanew

  // ---- PE signature + COFF header ----
  const coff = Buffer.alloc(24);
  coff.writeUInt32LE(spec.brokenSignature === true ? 0 : 0x00004550, 0);
  coff.writeUInt16LE(is64 ? 0x8664 : 0x014c, 4); // Machine
  coff.writeUInt16LE(placed.length, 6);
  coff.writeUInt16LE(optionalSize, 20);

  // ---- Optional header ----
  const optional = Buffer.alloc(optionalSize);
  optional.writeUInt16LE(is64 ? 0x20b : 0x10b, 0);
  optional.writeUInt32LE(SECTION_ALIGNMENT, 32); // SectionAlignment
  optional.writeUInt32LE(FILE_ALIGNMENT, 36); // FileAlignment
  const directoryBase = is64 ? 112 : 96;
  optional.writeUInt32LE(16, directoryBase - 4); // NumberOfRvaAndSizes
  if (importRva) {
    optional.writeUInt32LE(importRva, directoryBase + 1 * 8);
    optional.writeUInt32LE(importSection.importTableSize, directoryBase + 1 * 8 + 4);
  }
  if (delayRva) {
    optional.writeUInt32LE(delayRva, directoryBase + 13 * 8);
    optional.writeUInt32LE(importSection.delayTableSize, directoryBase + 13 * 8 + 4);
  }

  // ---- Section table ----
  const table = Buffer.alloc(placed.length * 40);
  placed.forEach((section, index) => {
    const at = index * 40;
    Buffer.from(section.name.slice(0, 8), 'latin1').copy(table, at);
    table.writeUInt32LE(Math.max(section.data.length, 1), at + 8); // VirtualSize
    table.writeUInt32LE(section.virtualAddress, at + 12);
    table.writeUInt32LE(section.rawSize, at + 16);
    table.writeUInt32LE(section.rawOffset, at + 20);
    table.writeUInt32LE(0x40000040, at + 36); // initialised, readable
  });

  // ---- Assemble ----
  const total = rawOffset + (spec.overlay?.length ?? 0);
  const image = Buffer.alloc(total);
  dos.copy(image, 0);
  coff.copy(image, 0x40);
  optional.copy(image, 0x40 + 24);
  table.copy(image, 0x40 + 24 + optionalSize);
  for (const section of placed) section.data.copy(image, section.rawOffset);
  spec.overlay?.copy(image, rawOffset);
  return image;
}

interface ImportSection {
  data: Buffer;
  importTableOffset: number;
  importTableSize: number;
  delayTableOffset: number;
  delayTableSize: number;
}

/**
 * Build the import and delay-import descriptor tables plus their name
 * strings. Descriptor name fields hold RVAs, and the section is placed at a
 * known virtual address, so the offset within the section is the RVA delta.
 */
function buildImportSection(imports: string[], delayImports: string[]): ImportSection {
  if (imports.length === 0 && delayImports.length === 0) {
    return {
      data: Buffer.alloc(0),
      importTableOffset: 0,
      importTableSize: 0,
      delayTableOffset: 0,
      delayTableSize: 0
    };
  }

  const importTableSize = imports.length > 0 ? (imports.length + 1) * 20 : 0;
  const delayTableSize = delayImports.length > 0 ? (delayImports.length + 1) * 32 : 0;
  const importTableOffset = 0;
  const delayTableOffset = importTableSize;
  const namesOffset = importTableSize + delayTableSize;

  const nameOffsets = new Map<string, number>();
  let cursor = namesOffset;
  for (const name of [...imports, ...delayImports]) {
    if (nameOffsets.has(name)) continue;
    nameOffsets.set(name, cursor);
    cursor += Buffer.byteLength(name, 'latin1') + 1;
  }

  const data = Buffer.alloc(cursor);
  imports.forEach((name, index) => {
    // Descriptor: name RVA sits at offset 12 of a 20-byte entry.
    data.writeUInt32LE(SECTION_ALIGNMENT + (nameOffsets.get(name) as number), importTableOffset + index * 20 + 12);
  });
  delayImports.forEach((name, index) => {
    // Delay descriptor: name RVA sits at offset 4 of a 32-byte entry.
    data.writeUInt32LE(SECTION_ALIGNMENT + (nameOffsets.get(name) as number), delayTableOffset + index * 32 + 4);
  });
  for (const [name, offset] of nameOffsets) {
    Buffer.from(`${name}\0`, 'latin1').copy(data, offset);
  }

  return { data, importTableOffset, importTableSize, delayTableOffset, delayTableSize };
}
