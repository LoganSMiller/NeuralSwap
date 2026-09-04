use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use std::thread::sleep;
use std::time::Duration;

use crate::error::{fail, Code, Result};

/// Durable replace.
///
/// A plain write truncates the target first, so a crash or a full disk
/// part-way through leaves a half-written file where a valid one used to be -
/// which is precisely how a settings file becomes unreadable.
///
/// Write to a sibling temp file, flush it to the platter, then rename over the
/// target. Rename is atomic on NTFS and on POSIX, so a reader sees either the
/// whole old file or the whole new one and never a torn mixture.
pub fn write_atomic(file: &Path, data: &[u8]) -> Result<()> {
    replace_atomic(file, |handle| handle.write_all(data))
}

/// Durable replace of one file by another, streaming.
///
/// The same guarantee as [`write_atomic`], for content too large to want in
/// memory: a runtime DLL can be tens of megabytes, and several of them in one
/// install is a lot of allocation for no benefit when the bytes are only being
/// moved from one file to another.
pub fn copy_atomic(from: &Path, to: &Path) -> Result<()> {
    let mut source = File::open(from).map_err(|error| {
        crate::Error::new(
            Code::StateUnwritable,
            format!("could not open {}: {error}", from.display()),
        )
    })?;
    replace_atomic(to, |handle| std::io::copy(&mut source, handle).map(|_| ()))
}

/// Write a sibling temp file with `fill`, flush it to the platter, then rename
/// it over the target.
///
/// Rename is atomic on NTFS and on POSIX, so a reader sees either the whole
/// old file or the whole new one and never a torn mixture. The temp file is
/// removed on every failure path, including a failed rename, so a refused
/// write leaves no litter beside the target.
fn replace_atomic<F>(file: &Path, fill: F) -> Result<()>
where
    F: FnOnce(&mut File) -> std::io::Result<()>,
{
    let parent = file.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(parent).map_err(map_io("create parent directory", file))?;

    // A unique suffix means two writers cannot collide on the temp path.
    let temp = parent.join(format!(
        "{}.{}.{}.tmp",
        file.file_name().and_then(|n| n.to_str()).unwrap_or("state"),
        std::process::id(),
        unique()
    ));

    let write = || -> std::io::Result<()> {
        let mut handle = File::create(&temp)?;
        fill(&mut handle)?;
        handle.flush()?;
        // Flush the file's own contents before the rename makes it visible.
        handle.sync_all()?;
        Ok(())
    };
    if let Err(error) = write() {
        let _ = fs::remove_file(&temp);
        return fail(
            Code::StateUnwritable,
            format!("could not write {}: {error}", file.display()),
        );
    }

    if let Err(error) = rename_with_retry(&temp, file) {
        let _ = fs::remove_file(&temp);
        return Err(error);
    }
    Ok(())
}

pub fn write_json_atomic<T: serde::Serialize>(file: &Path, value: &T) -> Result<()> {
    let mut text = serde_json::to_string_pretty(value)
        .map_err(|error| crate::Error::new(Code::StateUnwritable, format!("serialise: {error}")))?;
    text.push('\n');
    write_atomic(file, text.as_bytes())
}

/// Windows can transiently refuse to replace a file that something else has
/// open for a moment - Defender scanning the bytes just flushed, Search
/// indexing them, a backup agent, or Explorer previewing the folder. The
/// failure clears in milliseconds.
///
/// Treating it as fatal is wrong: nothing is broken, and the user would be
/// told their settings could not be saved because an antivirus blinked. This
/// is not hypothetical - it showed up as intermittent failures while building
/// the reference implementation on this very machine.
const RENAME_ATTEMPTS: u32 = 10;

fn transient(error: &std::io::Error) -> bool {
    if error.kind() == std::io::ErrorKind::PermissionDenied
        || error.kind() == std::io::ErrorKind::NotFound
    {
        return true;
    }
    // ERROR_ACCESS_DENIED, ERROR_SHARING_VIOLATION, ERROR_LOCK_VIOLATION.
    matches!(error.raw_os_error(), Some(5) | Some(32) | Some(33))
}

fn rename_with_retry(from: &Path, to: &Path) -> Result<()> {
    let mut attempt = 1;
    loop {
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(error) if attempt < RENAME_ATTEMPTS && transient(&error) => {
                // 1, 2, 4 ... 256 ms - about half a second in total.
                sleep(Duration::from_millis(1u64 << (attempt - 1)));
                attempt += 1;
            }
            Err(error) => {
                return fail(
                    Code::StateUnwritable,
                    format!("could not replace {}: {error}", to.display()),
                );
            }
        }
    }
}

fn map_io<'a>(what: &'static str, file: &'a Path) -> impl Fn(std::io::Error) -> crate::Error + 'a {
    move |error| {
        crate::Error::new(
            Code::StateUnwritable,
            format!("{what} for {}: {error}", file.display()),
        )
    }
}

/// A short unique token for temp names. Avoids pulling in a uuid dependency
/// for something that only needs to not collide within a directory.
fn unique() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("{:x}{:x}", nanos, COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// Read a file, distinguishing "absent" from "unreadable".
///
/// Conflating the two is how a real failure becomes a silent reset: the
/// upstream project returned blank defaults on any read error at all.
pub fn read_to_string_or_none(file: &Path) -> Result<Option<String>> {
    match fs::read_to_string(file) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => fail(
            Code::StateCorrupt,
            format!("could not read {}: {error}", file.display()),
        ),
    }
}
