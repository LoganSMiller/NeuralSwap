//! A read-only PE inspector: imports, architecture, version resource, and a
//! bounded search for entry-point strings.
//!
//! Nothing here executes or loads anything. It only reads bytes.
//!
//! Why a struct rather than the five standalone functions it replaces:
//! identifying one game executable needs its architecture, its import table,
//! its version resource and sometimes a string scan. Implemented as separate
//! functions, each one opens the file, re-reads the DOS and COFF headers and
//! re-walks the section table, then closes it - so a single candidate costs
//! four or five opens and four or five header parses, for every executable in
//! every folder of a library.
//!
//! Here the file is opened once, the headers are parsed once, and each answer
//! is memoised.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use memchr::memmem;

use crate::bytes::Le;

const DOS_MAGIC: u16 = 0x5a4d; // 'MZ'
const PE_MAGIC: u32 = 0x0000_4550; // 'PE\0\0'
const PE32: u16 = 0x10b;
const PE32PLUS: u16 = 0x20b;

const DIR_IMPORT: usize = 1;
const DIR_RESOURCE: usize = 2;
const DIR_DELAY_IMPORT: usize = 13;
const RT_VERSION: u32 = 16;

/// Chunk size for the section scan. Large enough that the syscall overhead is
/// irrelevant, small enough not to hold a megabyte per concurrent scan.
const SCAN_CHUNK: usize = 1024 * 1024;

#[derive(Debug, Clone)]
struct Section {
    virtual_size: u32,
    virtual_address: u32,
    raw_size: u32,
    raw_offset: u32,
}

#[derive(Debug, Clone)]
struct Directory {
    rva: u32,
    size: u32,
}

#[derive(Debug, Clone)]
struct Headers {
    is64: bool,
    machine: u16,
    directories: Vec<Directory>,
    sections: Vec<Section>,
}

pub struct PeFile {
    path: PathBuf,
    file: File,
    size: u64,
    headers: Headers,
    imports: Option<Vec<String>>,
    /// `Some(None)` records "this file has no version resource", which is
    /// worth remembering so a second question does not re-walk the tree.
    version_blob: Option<Option<Vec<u8>>>,
    bytes_read: u64,
}

impl PeFile {
    /// Returns `None` for anything that is not a readable PE image.
    pub fn open(path: &Path) -> Option<Self> {
        let mut file = File::open(path).ok()?;
        let size = file.metadata().ok()?.len();
        let (headers, consumed) = read_headers(&mut file)?;
        Some(Self {
            path: path.to_path_buf(),
            file,
            size,
            headers,
            imports: None,
            version_blob: None,
            bytes_read: consumed,
        })
    }

    /// Open, hand to `use_it`, and drop - the shape most callers want.
    pub fn with<T>(path: &Path, use_it: impl FnOnce(&mut Self) -> T, fallback: T) -> T {
        match Self::open(path) {
            Some(mut pe) => use_it(&mut pe),
            None => fallback,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 32 or 64, from the optional-header magic. This is authoritative and
    /// needs no heuristics: PE32 is 32-bit and PE32+ is 64-bit.
    pub fn bitness(&self) -> u8 {
        if self.headers.is64 {
            64
        } else {
            32
        }
    }

    pub fn machine(&self) -> u16 {
        self.headers.machine
    }

    /// Total bytes pulled off this file so far.
    ///
    /// Timing a parser is at the mercy of the OS page cache; bytes read is
    /// deterministic, which makes it the honest measure of what a scan
    /// strategy costs - and the vectors assert it for the overlay case.
    pub fn bytes_read(&self) -> u64 {
        self.bytes_read
    }

    /// Names of the DLLs this image links against, lower-cased.
    ///
    /// Delay-loaded imports are included: plenty of games bind d3d12 that way,
    /// and omitting them makes a DirectX 12 title look like it uses no
    /// graphics API at all.
    pub fn import_names(&mut self) -> Vec<String> {
        if let Some(cached) = &self.imports {
            return cached.clone();
        }
        let mut names = self.name_table(DIR_IMPORT, 20, 12);
        for name in self.name_table(DIR_DELAY_IMPORT, 32, 4) {
            if !names.contains(&name) {
                names.push(name);
            }
        }
        self.imports = Some(names.clone());
        names
    }

    /// "310.8.0.0" style version from the resource, or `None`.
    pub fn file_version(&mut self) -> Option<String> {
        let blob = self.version()?;

        // VS_FIXEDFILEINFO, found by its signature rather than a fixed offset.
        const SIGNATURE: [u8; 4] = [0xbd, 0x04, 0xef, 0xfe];
        if let Some(at) = memmem::find(&blob, &SIGNATURE) {
            if let (Some(ms), Some(ls)) = (blob.u32_at(at + 8), blob.u32_at(at + 12)) {
                let fixed = format!("{}.{}.{}.{}", ms >> 16, ms & 0xffff, ls >> 16, ls & 0xffff);
                if fixed != "0.0.0.0" {
                    return Some(fixed);
                }
            }
        }

        // Some vendor DLLs leave the fixed fields blank but still publish a
        // version in the string table. Reporting "no version" for those is
        // what makes a perfectly identifiable runtime look unknown.
        for key in ["FileVersion", "ProductVersion"] {
            let needle = utf16(&format!("{key}\0"));
            let Some(at) = memmem::find(&blob, &needle) else {
                continue;
            };
            // Values are 32-bit aligned after the key.
            let start = (at + needle.len() + 3) & !3;
            let mut end = start;
            while blob.u16_at(end).is_some_and(|unit| unit != 0) {
                end += 2;
            }
            let Some(raw) = blob.window(start, end.saturating_sub(start)) else {
                continue;
            };
            if let Some(version) = first_version(&from_utf16(raw)) {
                return Some(version);
            }
        }
        None
    }

    /// True when the version resource mentions `text`. Resource strings are
    /// UTF-16, which is why a plain byte search for ASCII would miss them.
    pub fn version_mentions(&mut self, text: &str) -> bool {
        match self.version() {
            Some(blob) => memmem::find(&blob, &utf16(text)).is_some(),
            None => false,
        }
    }

    /// Which of `markers` appear as ASCII anywhere in the image's mapped
    /// sections.
    ///
    /// A game that reaches Direct3D through `LoadLibrary` has no import entry
    /// for it, but the DLL name and the entry point it asks for are still
    /// sitting in the binary as plain strings.
    ///
    /// Only mapped sections are searched, not the whole file. Appended overlay
    /// data - the payload of a self-extracting installer, or the assets some
    /// engines bolt onto the executable - can be hundreds of megabytes and
    /// cannot contain a marker the loader will ever resolve. Reading it is
    /// pure cost, and on a large library it is most of the scan time.
    pub fn find_markers(&mut self, markers: &[&str]) -> Vec<String> {
        let mut found: Vec<String> = Vec::new();
        if markers.is_empty() {
            return found;
        }
        // One prepared searcher per marker, reused across every chunk.
        //
        // This used to be `haystack.windows(n).position(..)`, which is a
        // byte-at-a-time comparison per marker per chunk. On Cyberpunk's 58 MB
        // executable that took six seconds; `memmem` is SIMD-accelerated and
        // already in the dependency tree, so hand-rolling it bought nothing
        // but the six seconds.
        let finders: Vec<memmem::Finder<'_>> = markers
            .iter()
            .map(|marker| memmem::Finder::new(marker.as_bytes()))
            .collect();
        let longest = markers.iter().map(|marker| marker.len()).max().unwrap_or(0);

        for span in self.search_spans() {
            self.scan(span.0, span.1, longest, |view| {
                for (index, finder) in finders.iter().enumerate() {
                    let Some(marker) = markers.get(index) else {
                        continue;
                    };
                    if !found.iter().any(|f| f == marker) && finder.find(view).is_some() {
                        found.push((*marker).to_owned());
                    }
                }
                found.len() == finders.len()
            });
            if found.len() == finders.len() {
                break;
            }
        }
        found.sort();
        found
    }

    /// True when the mapped sections contain `needle`.
    pub fn contains_bytes(&mut self, needle: &[u8]) -> bool {
        if needle.is_empty() {
            return false;
        }
        let finder = memmem::Finder::new(needle);
        let mut hit = false;
        for span in self.search_spans() {
            self.scan(span.0, span.1, needle.len(), |view| {
                if finder.find(view).is_some() {
                    hit = true;
                }
                hit
            });
            if hit {
                break;
            }
        }
        hit
    }

    // ---- internals ----

    fn read_at(&mut self, length: usize, position: u64) -> Vec<u8> {
        if length == 0 {
            return Vec::new();
        }
        if self.file.seek(SeekFrom::Start(position)).is_err() {
            return Vec::new();
        }
        let mut buffer = vec![0u8; length];
        let mut filled = 0;
        while filled < length {
            let Some(target) = buffer.get_mut(filled..) else {
                break;
            };
            match self.file.read(target) {
                Ok(0) => break,
                Ok(read) => filled += read,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        buffer.truncate(filled);
        self.bytes_read = self.bytes_read.saturating_add(filled as u64);
        buffer
    }

    /// Byte ranges worth searching, merged and clamped to the file, in file
    /// order so a marker near the front is found early.
    fn search_spans(&self) -> Vec<(u64, u64)> {
        let mut spans: Vec<(u64, u64)> = self
            .headers
            .sections
            .iter()
            .filter(|section| section.raw_size > 0 && section.raw_offset > 0)
            .filter_map(|section| {
                let offset = u64::from(section.raw_offset);
                let available = self.size.checked_sub(offset)?;
                let length = std::cmp::min(u64::from(section.raw_size), available);
                (length > 0).then_some((offset, length))
            })
            .collect();
        spans.sort_unstable();

        // A malformed or packed image can describe no usable sections at all;
        // fall back to the whole file rather than silently finding nothing.
        if spans.is_empty() {
            return vec![(0, self.size)];
        }

        let mut merged: Vec<(u64, u64)> = Vec::new();
        for (offset, length) in spans {
            match merged.last_mut() {
                Some(last) if offset <= last.0.saturating_add(last.1) => {
                    let end = offset.saturating_add(length);
                    last.1 = std::cmp::max(last.1, end.saturating_sub(last.0));
                }
                _ => merged.push((offset, length)),
            }
        }
        merged
    }

    /// Walk a byte range in chunks, carrying `overlap` bytes across each
    /// boundary so a marker straddling two chunks is still found. `visit`
    /// returns true to stop early.
    fn scan(
        &mut self,
        offset: u64,
        length: u64,
        overlap: usize,
        mut visit: impl FnMut(&[u8]) -> bool,
    ) {
        let mut carried: Vec<u8> = Vec::new();
        let mut position = offset;
        let end = offset.saturating_add(length);

        while position < end {
            let want = std::cmp::min(SCAN_CHUNK as u64, end - position);
            let chunk = self.read_at(usize::try_from(want).unwrap_or(0), position);
            if chunk.is_empty() {
                return;
            }
            let read = chunk.len() as u64;

            let view: Vec<u8> = if carried.is_empty() {
                chunk
            } else {
                let mut joined = std::mem::take(&mut carried);
                joined.extend_from_slice(&chunk);
                joined
            };
            if visit(&view) {
                return;
            }
            let keep = std::cmp::min(overlap, view.len());
            carried = view.get(view.len() - keep..).unwrap_or(&[]).to_vec();
            position = position.saturating_add(read);
        }
    }

    fn rva_to_offset(&self, rva: u32) -> Option<u64> {
        for section in &self.headers.sections {
            let span = std::cmp::max(section.virtual_size, section.raw_size);
            let start = section.virtual_address;
            if rva >= start && rva < start.saturating_add(span) {
                return Some(u64::from(section.raw_offset) + u64::from(rva - start));
            }
        }
        None
    }

    fn c_string(&mut self, position: u64) -> String {
        let buffer = self.read_at(256, position);
        let end = buffer
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(buffer.len());
        buffer
            .get(..end)
            .map(|bytes| bytes.iter().map(|b| char::from(*b)).collect())
            .unwrap_or_default()
    }

    fn name_table(&mut self, index: usize, stride: usize, name_field: usize) -> Vec<String> {
        let Some(directory) = self.headers.directories.get(index).cloned() else {
            return Vec::new();
        };
        if directory.rva == 0 {
            return Vec::new();
        }
        let Some(start) = self.rva_to_offset(directory.rva) else {
            return Vec::new();
        };

        let want = if directory.size == 0 {
            4096
        } else {
            std::cmp::min(directory.size as usize, 64 * 1024)
        };
        let table = self.read_at(want, start);

        let mut names = Vec::new();
        let mut offset = 0usize;
        while offset + stride <= table.len() {
            let Some(rva) = table.u32_at(offset + name_field) else {
                break;
            };
            // A zeroed descriptor terminates the table.
            if rva == 0 {
                break;
            }
            if let Some(at) = self.rva_to_offset(rva) {
                names.push(self.c_string(at).to_lowercase());
            }
            offset += stride;
        }
        names
    }

    fn version(&mut self) -> Option<Vec<u8>> {
        if let Some(cached) = &self.version_blob {
            return cached.clone();
        }
        let blob = self.read_version_blob();
        self.version_blob = Some(blob.clone());
        blob
    }

    fn read_version_blob(&mut self) -> Option<Vec<u8>> {
        let directory = self.headers.directories.get(DIR_RESOURCE)?.clone();
        if directory.rva == 0 {
            return None;
        }
        let base = self.rva_to_offset(directory.rva)?;

        // type (RT_VERSION) -> name -> language -> data entry. The high bit
        // marks an offset as pointing at a subdirectory rather than at data.
        let type_entry = self
            .resource_entries(base, 0)
            .into_iter()
            .find(|entry| entry.0 & 0x7fff_ffff == RT_VERSION && entry.1 & 0x8000_0000 != 0)?;
        let name_entry = self
            .resource_entries(base, type_entry.1 & 0x7fff_ffff)
            .into_iter()
            .next()?;
        if name_entry.1 & 0x8000_0000 == 0 {
            return None;
        }
        let language = self
            .resource_entries(base, name_entry.1 & 0x7fff_ffff)
            .into_iter()
            .next()?;

        let entry = self.read_at(16, base + u64::from(language.1));
        let data_rva = entry.u32_at(0)?;
        let data_size = entry.u32_at(4)?;
        if data_size == 0 {
            return None;
        }
        let at = self.rva_to_offset(data_rva)?;
        let want = std::cmp::min(data_size as usize, 64 * 1024);
        Some(self.read_at(want, at))
    }

    /// `(id, offset)` pairs from one resource directory.
    fn resource_entries(&mut self, base: u64, offset: u32) -> Vec<(u32, u32)> {
        let header = self.read_at(16, base + u64::from(offset));
        let (Some(named), Some(by_id)) = (header.u16_at(12), header.u16_at(14)) else {
            return Vec::new();
        };
        let count = usize::from(named) + usize::from(by_id);
        // A wild count would mean a huge pointless read.
        if count > 4096 {
            return Vec::new();
        }
        let raw = self.read_at(count * 8, base + u64::from(offset) + 16);

        let mut out = Vec::with_capacity(count);
        let mut at = 0usize;
        while at + 8 <= raw.len() {
            if let (Some(id), Some(child)) = (raw.u32_at(at), raw.u32_at(at + 4)) {
                out.push((id, child));
            }
            at += 8;
        }
        out
    }
}

/// Parse the headers, returning them and the bytes consumed doing so.
fn read_headers(file: &mut File) -> Option<(Headers, u64)> {
    let mut consumed = 0u64;
    let mut read = |file: &mut File, length: usize, position: u64| -> Vec<u8> {
        if length == 0 || file.seek(SeekFrom::Start(position)).is_err() {
            return Vec::new();
        }
        let mut buffer = vec![0u8; length];
        let got = file.read(&mut buffer).unwrap_or(0);
        buffer.truncate(got);
        consumed = consumed.saturating_add(got as u64);
        buffer
    };

    let dos = read(file, 0x40, 0);
    if dos.u16_at(0)? != DOS_MAGIC {
        return None;
    }
    let pe_offset = u64::from(dos.u32_at(0x3c)?);

    // Signature (4) plus the COFF file header (20).
    let coff = read(file, 24, pe_offset);
    if coff.u32_at(0)? != PE_MAGIC {
        return None;
    }
    let machine = coff.u16_at(4)?;
    let section_count = coff.u16_at(6)?;
    let optional_size = coff.u16_at(20)?;
    let optional_offset = pe_offset + 24;

    let optional = read(file, usize::from(optional_size), optional_offset);
    let magic = optional.u16_at(0)?;
    if magic != PE32 && magic != PE32PLUS {
        return None;
    }
    let is64 = magic == PE32PLUS;

    let directory_base = if is64 { 112 } else { 96 };
    let mut directories = Vec::with_capacity(16);
    for index in 0..16usize {
        let at = directory_base + index * 8;
        match (optional.u32_at(at), optional.u32_at(at + 4)) {
            (Some(rva), Some(size)) => directories.push(Directory { rva, size }),
            _ => break,
        }
    }

    // PE caps images at 96 sections; anything wilder is not worth reading.
    if section_count > 256 {
        return None;
    }
    let table = read(
        file,
        usize::from(section_count) * 40,
        optional_offset + u64::from(optional_size),
    );
    let mut sections = Vec::with_capacity(usize::from(section_count));
    for index in 0..usize::from(section_count) {
        let at = index * 40;
        let (Some(virtual_size), Some(virtual_address), Some(raw_size), Some(raw_offset)) = (
            table.u32_at(at + 8),
            table.u32_at(at + 12),
            table.u32_at(at + 16),
            table.u32_at(at + 20),
        ) else {
            break;
        };
        sections.push(Section {
            virtual_size,
            virtual_address,
            raw_size,
            raw_offset,
        });
    }

    Some((
        Headers {
            is64,
            machine,
            directories,
            sections,
        },
        consumed,
    ))
}

fn utf16(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

fn from_utf16(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .filter_map(|pair| Some(u16::from_le_bytes([*pair.first()?, *pair.get(1)?])))
        .collect();
    String::from_utf16_lossy(&units)
}

/// The first dotted or comma-separated version number in a string, normalised
/// to dots. Vendor resources write "1, 2, 3, 4" as often as "1.2.3.4".
fn first_version(text: &str) -> Option<String> {
    let bytes: Vec<char> = text.chars().collect();
    let mut index = 0usize;
    while index < bytes.len() {
        if !bytes.get(index).is_some_and(|c| c.is_ascii_digit()) {
            index += 1;
            continue;
        }
        let start = index;
        let mut parts = 1;
        let mut end = index;
        while end < bytes.len() {
            let character = *bytes.get(end)?;
            if character.is_ascii_digit() {
                end += 1;
            } else if character == '.' || character == ',' || character == ' ' {
                // Look past separators and whitespace for another group.
                let mut probe = end;
                while bytes.get(probe).is_some_and(|c| *c == ' ') {
                    probe += 1;
                }
                if bytes.get(probe).is_some_and(|c| *c == '.' || *c == ',') {
                    probe += 1;
                }
                while bytes.get(probe).is_some_and(|c| *c == ' ') {
                    probe += 1;
                }
                if probe > end && bytes.get(probe).is_some_and(char::is_ascii_digit) {
                    parts += 1;
                    end = probe;
                } else {
                    break;
                }
            } else {
                break;
            }
        }
        if parts >= 2 {
            let raw: String = bytes.get(start..end)?.iter().collect();
            let normalised: Vec<String> = raw
                .split(['.', ','])
                .map(|part| part.trim().to_owned())
                .filter(|part| !part.is_empty())
                .collect();
            return Some(normalised.join("."));
        }
        index = end.max(start + 1);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::first_version;

    #[test]
    fn version_strings_are_normalised() {
        assert_eq!(first_version("1.2.3.4"), Some("1.2.3.4".to_owned()));
        assert_eq!(first_version("v3.7.20"), Some("3.7.20".to_owned()));
        // Vendor resources really do write it this way.
        assert_eq!(first_version("1, 2, 3, 4"), Some("1.2.3.4".to_owned()));
        assert_eq!(first_version("build 12.4 final"), Some("12.4".to_owned()));
        // A lone number is not a version.
        assert_eq!(first_version("42"), None);
        assert_eq!(first_version("no digits here"), None);
    }
}
