use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

use crate::bytes::Le;
use crate::error::{fail, Code, Result};

const EOCD_SIG: u32 = 0x0605_4b50;
const EOCD64_SIG: u32 = 0x0606_4b50;
const EOCD64_LOCATOR_SIG: u32 = 0x0706_4b50;
const CENTRAL_SIG: u32 = 0x0201_4b50;
const LOCAL_SIG: u32 = 0x0403_4b50;

const EOCD_MIN: usize = 22;
const MAX_COMMENT: usize = 0xffff;

/// Unix file-type mask and the symlink type, as stored in `st_mode`.
const S_IFMT: u16 = 0xf000;
const S_IFLNK: u16 = 0xa000;

#[derive(Debug, Clone)]
pub struct ZipEntry {
    pub name: String,
    pub method: u16,
    pub crc32: u32,
    pub compressed_size: u64,
    pub uncompressed_size: u64,
    pub local_header_offset: u64,
    /// Unix `st_mode` when the archive records one.
    pub unix_mode: Option<u16>,
    pub is_directory: bool,
    pub is_symlink: bool,
    /// Bit 0 of the general-purpose flags.
    pub encrypted: bool,
}

/// The shared `Le` accessors return `Option`; a malformed archive is an error
/// with a code, so they are adapted once here rather than at thirty call
/// sites. The bounds checking itself stays in `crate::bytes`.
trait LeZip {
    fn zu16(&self, offset: usize) -> Result<u16>;
    fn zu32(&self, offset: usize) -> Result<u32>;
    fn zu64(&self, offset: usize) -> Result<u64>;
}

impl LeZip for [u8] {
    // Qualified calls, so these cannot resolve back to themselves.
    fn zu16(&self, offset: usize) -> Result<u16> {
        match Le::u16_at(self, offset) {
            Some(value) => Ok(value),
            None => fail(Code::ZipInvalid, "truncated: expected 2 bytes"),
        }
    }

    fn zu32(&self, offset: usize) -> Result<u32> {
        match Le::u32_at(self, offset) {
            Some(value) => Ok(value),
            None => fail(Code::ZipInvalid, "truncated: expected 4 bytes"),
        }
    }

    fn zu64(&self, offset: usize) -> Result<u64> {
        match Le::u64_at(self, offset) {
            Some(value) => Ok(value),
            None => fail(Code::ZipInvalid, "truncated: expected 8 bytes"),
        }
    }
}

fn read_chunk(file: &mut File, length: usize, position: u64) -> Result<Vec<u8>> {
    if length == 0 {
        return Ok(Vec::new());
    }
    file.seek(SeekFrom::Start(position))
        .map_err(|error| crate::Error::new(Code::ZipInvalid, format!("seek failed: {error}")))?;
    let mut buffer = vec![0u8; length];
    let mut filled = 0;
    while filled < length {
        match file.read(buffer.get_mut(filled..).unwrap_or(&mut [])) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => {
                return fail(Code::ZipInvalid, format!("read failed: {error}"));
            }
        }
    }
    buffer.truncate(filled);
    Ok(buffer)
}

/// Locate the End Of Central Directory record by scanning backwards: its
/// position is not fixed, because a ZIP may carry a comment of up to 64 KiB.
fn find_eocd(file: &mut File, size: u64) -> Result<(Vec<u8>, u64)> {
    let window = std::cmp::min(size, (EOCD_MIN + MAX_COMMENT) as u64);
    let start = size - window;
    let tail = read_chunk(file, usize::try_from(window).unwrap_or(0), start)?;
    if tail.len() < EOCD_MIN {
        return fail(Code::ZipInvalid, "file is shorter than an EOCD record");
    }

    for index in (0..=tail.len() - EOCD_MIN).rev() {
        if tail.zu32(index)? != EOCD_SIG {
            continue;
        }
        let comment_length = usize::from(tail.zu16(index + 20)?);
        // The comment length must account for exactly the remaining bytes,
        // which is what distinguishes a real EOCD from those four bytes
        // happening to appear inside compressed data.
        if index + EOCD_MIN + comment_length == tail.len() {
            let record = tail.get(index..).unwrap_or(&[]).to_vec();
            return Ok((record, start + index as u64));
        }
    }
    fail(Code::ZipInvalid, "no end-of-central-directory record")
}

struct Directory {
    entry_count: u64,
    central_offset: u64,
    central_size: u64,
}

fn read_directory_location(file: &mut File, size: u64) -> Result<Directory> {
    let (eocd, eocd_offset) = find_eocd(file, size)?;
    let mut entry_count = u64::from(eocd.zu16(10)?);
    let mut central_size = u64::from(eocd.zu32(12)?);
    let mut central_offset = u64::from(eocd.zu32(16)?);

    // 0xffff / 0xffffffff are the Zip64 sentinels: the real values live in the
    // Zip64 record, reached through a 20-byte locator just before the EOCD.
    let needs_zip64 =
        entry_count == 0xffff || central_size == 0xffff_ffff || central_offset == 0xffff_ffff;
    if needs_zip64 && eocd_offset >= 20 {
        let locator = read_chunk(file, 20, eocd_offset - 20)?;
        if locator.len() == 20 && locator.zu32(0)? == EOCD64_LOCATOR_SIG {
            let zip64_offset = locator.zu64(8)?;
            let zip64 = read_chunk(file, 56, zip64_offset)?;
            if zip64.len() >= 56 && zip64.zu32(0)? == EOCD64_SIG {
                entry_count = zip64.zu64(32)?;
                central_size = zip64.zu64(40)?;
                central_offset = zip64.zu64(48)?;
            }
        }
    }

    if central_offset.saturating_add(central_size) > size {
        return fail(Code::ZipInvalid, "central directory lies outside the file");
    }
    Ok(Directory {
        entry_count,
        central_offset,
        central_size,
    })
}

/// Zip64 stores oversized fields in extra-field block 0x0001, present only for
/// the values that actually overflowed - so the block is read positionally in
/// the same order the 32-bit fields appear.
fn apply_zip64_extra(entry: &mut ZipEntry, extra: &[u8]) -> Result<()> {
    let mut offset = 0usize;
    while offset + 4 <= extra.len() {
        let id = extra.zu16(offset)?;
        let size = usize::from(extra.zu16(offset + 2)?);
        let body = extra.get(offset + 4..offset + 4 + size).unwrap_or(&[]);
        if id == 0x0001 {
            let mut cursor = 0usize;
            let next = |cursor: &mut usize| -> Option<u64> {
                let value = body.zu64(*cursor).ok()?;
                *cursor += 8;
                Some(value)
            };
            if entry.uncompressed_size == 0xffff_ffff {
                if let Some(value) = next(&mut cursor) {
                    entry.uncompressed_size = value;
                }
            }
            if entry.compressed_size == 0xffff_ffff {
                if let Some(value) = next(&mut cursor) {
                    entry.compressed_size = value;
                }
            }
            if entry.local_header_offset == 0xffff_ffff {
                if let Some(value) = next(&mut cursor) {
                    entry.local_header_offset = value;
                }
            }
            return Ok(());
        }
        offset += 4 + size;
    }
    Ok(())
}

/// Every entry described by the central directory, which is authoritative.
pub fn read_entries(file: &mut File, size: u64) -> Result<Vec<ZipEntry>> {
    let location = read_directory_location(file, size)?;
    let central = read_chunk(
        file,
        usize::try_from(location.central_size).unwrap_or(0),
        location.central_offset,
    )?;

    let mut entries: Vec<ZipEntry> = Vec::new();
    let mut offset = 0usize;

    while (entries.len() as u64) < location.entry_count {
        if offset + 46 > central.len() {
            break;
        }
        if central.zu32(offset)? != CENTRAL_SIG {
            return fail(
                Code::ZipInvalid,
                format!("bad central directory header at {offset}"),
            );
        }
        let version_made_by = central.zu16(offset + 4)?;
        let flags = central.zu16(offset + 8)?;
        let name_length = usize::from(central.zu16(offset + 28)?);
        let extra_length = usize::from(central.zu16(offset + 30)?);
        let comment_length = usize::from(central.zu16(offset + 32)?);
        let external_attributes = central.zu32(offset + 38)?;

        let name_start = offset + 46;
        let name_end = name_start + name_length;
        let Some(name_bytes) = central.get(name_start..name_end) else {
            return fail(Code::ZipInvalid, "truncated entry name");
        };
        // Bit 11 marks the name as UTF-8. Older archivers use the local
        // codepage; decoding as UTF-8 is the common convention and is lossless
        // for the ASCII paths every archive we consume actually uses.
        let name = String::from_utf8_lossy(name_bytes).into_owned();

        // Only the Unix host (3) puts st_mode in the high half of the attrs.
        let unix_mode = if version_made_by >> 8 == 3 {
            Some(((external_attributes >> 16) & 0xffff) as u16)
        } else {
            None
        };

        let mut entry = ZipEntry {
            is_directory: name.ends_with('/') || name.ends_with('\\'),
            is_symlink: unix_mode.is_some_and(|mode| mode & S_IFMT == S_IFLNK),
            encrypted: flags & 0x1 != 0,
            name,
            method: central.zu16(offset + 10)?,
            crc32: central.zu32(offset + 16)?,
            compressed_size: u64::from(central.zu32(offset + 20)?),
            uncompressed_size: u64::from(central.zu32(offset + 24)?),
            local_header_offset: u64::from(central.zu32(offset + 42)?),
            unix_mode,
        };
        let extra = central
            .get(name_end..name_end + extra_length)
            .unwrap_or(&[]);
        apply_zip64_extra(&mut entry, extra)?;

        entries.push(entry);
        offset = name_end + extra_length + comment_length;
    }

    Ok(entries)
}

/// Byte offset at which an entry's compressed data begins.
pub fn data_offset_of(file: &mut File, entry: &ZipEntry) -> Result<u64> {
    let header = read_chunk(file, 30, entry.local_header_offset)?;
    if header.len() < 30 || header.zu32(0)? != LOCAL_SIG {
        return fail(
            Code::ZipInvalid,
            format!("bad local file header: {}", entry.name),
        );
    }
    let name_length = u64::from(header.zu16(26)?);
    let extra_length = u64::from(header.zu16(28)?);
    Ok(entry.local_header_offset + 30 + name_length + extra_length)
}
