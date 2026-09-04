use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use flate2::read::DeflateDecoder;

use crate::error::{fail, Code, Result};
use crate::fsx::paths::{assert_safe_relative, safe_path};

use super::read::{data_offset_of, read_entries, ZipEntry};

const STORED: u16 = 0;
const DEFLATED: u16 = 8;

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Refuse an archive claiming more entries than this.
    pub max_entries: usize,
    /// Refuse a single entry larger than this once decompressed.
    pub max_entry_bytes: u64,
    /// Refuse an archive whose entries total more than this once decompressed.
    pub max_total_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_entries: 20_000,
            max_entry_bytes: 512 * 1024 * 1024,
            max_total_bytes: 2 * 1024 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Default)]
pub struct Extracted {
    pub files: Vec<String>,
    pub bytes: u64,
}

/// Extract a ZIP without any of the ways one can be made to write outside its
/// destination.
///
/// This is the Rust port of the extractor written to replace `extract-zip`,
/// whose entire published range carries GHSA-jmr9-qjv8-65gv - unvalidated
/// symlink path traversal - with no fixed version available. It was the
/// upstream project's only production dependency, and it was pointed at
/// archives fetched over the network and unpacked into the user's profile.
///
/// The rules, none of which `extract-zip` applies:
///
///   - Symlink entries are refused outright. Not resolved, not silently
///     skipped: a component archive has no legitimate reason to contain one,
///     so its presence means the archive is not what we think it is.
///   - Every name goes through the path validator, which rejects traversal,
///     rooted and UNC paths, alternate data streams, DOS device names and
///     trailing-dot ambiguity, and refuses to write through an existing
///     symlink or junction.
///   - Entry count and decompressed size are capped before inflating, so a
///     small archive cannot expand to fill the disk.
///   - Every entry's CRC-32 and length are verified against the central
///     directory as it is written, and a mismatch removes the partial file.
///   - Only stored and deflate are accepted. Encrypted entries are refused.
///
/// Validation of the whole archive completes before a single byte is written:
/// a half-applied extraction of a hostile archive is still a compromised
/// folder.
pub fn extract_zip(archive: &Path, destination: &Path, limits: Limits) -> Result<Extracted> {
    let mut file = File::open(archive)
        .map_err(|error| crate::Error::new(Code::ZipInvalid, format!("cannot open: {error}")))?;
    let size = file
        .metadata()
        .map_err(|error| crate::Error::new(Code::ZipInvalid, format!("cannot stat: {error}")))?
        .len();

    let entries = read_entries(&mut file, size)?;

    if entries.len() > limits.max_entries {
        return fail(
            Code::ZipTooLarge,
            format!(
                "{} entries exceeds the limit of {}",
                entries.len(),
                limits.max_entries
            ),
        );
    }

    let mut declared_total: u64 = 0;
    for entry in &entries {
        if entry.encrypted {
            return fail(
                Code::ZipUnsupported,
                format!("encrypted entry: {}", entry.name),
            );
        }
        if entry.is_symlink {
            return fail(
                Code::ZipEntryUnsafe,
                format!("archive contains a symlink: {}", entry.name),
            );
        }
        if entry.is_directory {
            // A directory entry still has to have a usable name.
            check_name(&entry.name, destination)?;
            continue;
        }
        if entry.method != STORED && entry.method != DEFLATED {
            return fail(
                Code::ZipUnsupported,
                format!(
                    "unsupported compression method {} in {}",
                    entry.method, entry.name
                ),
            );
        }
        if entry.uncompressed_size > limits.max_entry_bytes {
            return fail(
                Code::ZipTooLarge,
                format!(
                    "{} is {} bytes once decompressed",
                    entry.name, entry.uncompressed_size
                ),
            );
        }
        declared_total = declared_total.saturating_add(entry.uncompressed_size);
        check_name(&entry.name, destination)?;
    }
    if declared_total > limits.max_total_bytes {
        return fail(
            Code::ZipTooLarge,
            format!("{declared_total} bytes once decompressed exceeds the limit"),
        );
    }

    fs::create_dir_all(destination).map_err(io_err("create destination"))?;

    let mut result = Extracted::default();
    for entry in &entries {
        let relative = normalize_name(&entry.name);
        if entry.is_directory {
            let target = safe_path(destination, &relative)?;
            fs::create_dir_all(&target).map_err(io_err("create directory"))?;
            continue;
        }
        let target = safe_path(destination, &relative)?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(io_err("create parent"))?;
        }
        let written = write_entry(&mut file, entry, &target)?;
        result.bytes = result.bytes.saturating_add(written);
        if result.bytes > limits.max_total_bytes {
            return fail(
                Code::ZipTooLarge,
                format!(
                    "archive expanded past its declared size at {} bytes",
                    result.bytes
                ),
            );
        }
        result.files.push(relative);
    }

    Ok(result)
}

fn io_err(what: &'static str) -> impl Fn(std::io::Error) -> crate::Error {
    move |error| crate::Error::new(Code::ZipInvalid, format!("{what}: {error}"))
}

/// Reject a name before it becomes a path, with the raw name still in hand.
///
/// `assert_safe_relative` touches no filesystem, so it can be applied against
/// the real destination here without the synthetic root the TypeScript version
/// needed to avoid triggering the symlink walk early.
fn check_name(name: &str, destination: &Path) -> Result<()> {
    if name.is_empty() {
        return fail(Code::ZipEntryUnsafe, "empty entry name");
    }
    if name.contains('\0') {
        return fail(Code::ZipEntryUnsafe, "NUL byte in entry name");
    }
    assert_safe_relative(&normalize_name(name), destination).map(|_: PathBuf| ())
}

fn normalize_name(name: &str) -> String {
    name.replace('\\', "/").trim_end_matches('/').to_owned()
}

fn write_entry(archive: &mut File, entry: &ZipEntry, target: &Path) -> Result<u64> {
    let start = data_offset_of(archive, entry)?;
    archive
        .seek(SeekFrom::Start(start))
        .map_err(io_err("seek to entry data"))?;

    // Reading is bounded by the declared compressed size, so one entry cannot
    // consume the rest of the file.
    //
    // `Read::take` rather than a hand-rolled counter: an earlier version here
    // checked its budget *before* each read but handed the full buffer to the
    // inner reader, so a stored entry happily read past its own data and came
    // out too long. `take` clamps every read to what is left, which is the
    // part that is easy to get wrong and pointless to reimplement.
    let bounded = (&mut *archive).take(entry.compressed_size);

    let mut sink = File::create(target).map_err(io_err("create file"))?;
    let mut hasher = crc32fast::Hasher::new();
    let mut written: u64 = 0;

    let outcome = {
        let mut source: Box<dyn Read> = if entry.method == STORED {
            Box::new(bounded)
        } else {
            Box::new(DeflateDecoder::new(bounded))
        };
        copy_verified(&mut source, &mut sink, &mut hasher, &mut written)
    };

    // Any failure - IO, a corrupt deflate stream, a bad checksum - must not
    // leave a partial DLL sitting in somebody's game folder.
    let discard = |detail: String, code: Code| -> Result<u64> {
        drop(sink);
        let _ = fs::remove_file(target);
        fail(code, detail)
    };

    if let Err(error) = outcome {
        return discard(format!("{}: {error}", entry.name), Code::ZipInvalid);
    }
    if written != entry.uncompressed_size {
        return discard(
            format!(
                "{}: length {} does not match the directory's {}",
                entry.name, written, entry.uncompressed_size
            ),
            Code::ZipChecksum,
        );
    }
    if hasher.finalize() != entry.crc32 {
        return discard(
            format!("{}: CRC-32 does not match the directory", entry.name),
            Code::ZipChecksum,
        );
    }
    Ok(written)
}

fn copy_verified(
    source: &mut dyn Read,
    sink: &mut File,
    hasher: &mut crc32fast::Hasher,
    written: &mut u64,
) -> std::io::Result<()> {
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = match source.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        let chunk = buffer.get(..read).unwrap_or(&[]);
        hasher.update(chunk);
        sink.write_all(chunk)?;
        *written = written.saturating_add(read as u64);
    }
    sink.flush()
}
