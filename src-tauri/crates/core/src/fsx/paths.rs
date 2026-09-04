//! Path safety.
//!
//! Resolving a relative path against a game folder is the most dangerous
//! operation in this application: every install, backup and restore funnels
//! through it, and the relative parts come from on-disk manifests and archive
//! entries rather than from us.
//!
//! A lexical prefix test is not sufficient on Windows. Each of these is a real
//! way to leave a folder while still passing `dest.starts_with(root)`:
//!
//! - `..` segments - classic traversal
//! - `file.txt:evil` - NTFS alternate data stream
//! - `sub`, where `sub` is a junction - reparse point out of the tree
//! - `CON`, `NUL`, `COM1`, `LPT1.txt` - DOS device, writes to a device
//! - `sub.` or `sub ` - Win32 strips the trailing dot or space, so the path
//!   that was validated is not the path that gets opened
//! - a NUL byte - truncates the path in Win32 APIs
//!
//! Unlike the TypeScript version this replaces, the rules are applied without
//! consulting the host platform's path module. That version leaned on Node's
//! `path.isAbsolute`, which meant a UNC path like `\\server\share\evil.dll`
//! was refused on Windows and quietly accepted as an odd filename on Linux.
//! Deciding it here makes both platforms agree, and agree on the strict answer.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::error::{fail, Code, Result};

/// CON, PRN, AUX, NUL, COM0-9, LPT0-9 - reserved with or without an extension.
const RESERVED_STEMS: [&str; 4] = ["con", "prn", "aux", "nul"];

fn is_reserved(segment: &str) -> bool {
    // The whole segment up to the first dot is what Win32 matches against, so
    // `nul.txt` is reserved while `nullify.txt` is an ordinary file.
    let stem = segment
        .split('.')
        .next()
        .unwrap_or(segment)
        .to_ascii_lowercase();
    if RESERVED_STEMS.contains(&stem.as_str()) {
        return true;
    }
    for prefix in ["com", "lpt"] {
        if let Some(rest) = stem.strip_prefix(prefix) {
            if rest.len() == 1 && rest.as_bytes().first().is_some_and(u8::is_ascii_digit) {
                return true;
            }
        }
    }
    false
}

fn is_separator(character: char) -> bool {
    character == '/' || character == '\\'
}

/// Validate a relative path without touching the filesystem.
pub fn assert_safe_relative(rel: &str, root: &Path) -> Result<PathBuf> {
    if rel.is_empty() {
        return fail(Code::UnsafePath, "relative path must not be empty");
    }
    if rel.contains('\0') {
        return fail(Code::UnsafePath, "NUL byte in path");
    }
    // A drive-relative or rooted path is never valid here, and a colon
    // anywhere else names an alternate data stream.
    if rel.contains(':') {
        return fail(Code::UnsafePath, format!("colon in relative path: {rel}"));
    }
    if rel.starts_with(|c: char| is_separator(c)) {
        return fail(Code::UnsafePath, format!("rooted or UNC path: {rel}"));
    }

    let mut kept: Vec<&str> = Vec::new();
    for segment in rel.split(is_separator) {
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment == ".." {
            return fail(Code::UnsafePath, format!("path escapes the root: {rel}"));
        }
        if is_reserved(segment) {
            return fail(Code::ReservedName, format!("DOS device name: {segment}"));
        }
        // Win32 silently drops trailing dots and spaces, so `evil. ` and
        // `evil` are the same file to the OS but different strings to us.
        if segment.ends_with('.') || segment.ends_with(' ') {
            return fail(
                Code::UnsafePath,
                format!("trailing dot or space: {segment}"),
            );
        }
        kept.push(segment);
    }

    if kept.is_empty() {
        return fail(Code::OutsideRoot, "resolves to the root itself");
    }

    let mut dest = absolute(root);
    for segment in kept {
        dest.push(segment);
    }
    Ok(dest)
}

/// Validate a relative path *and* prove that no existing component between the
/// root and the target is a symlink or junction. Rust's `symlink_metadata`
/// reports both as symlinks on Windows, which is exactly the set to refuse.
///
/// Components that do not exist yet are fine - they cannot redirect a write -
/// but the walk continues past them to reach the ones that do.
pub fn safe_path(root: &Path, rel: &str) -> Result<PathBuf> {
    let dest = assert_safe_relative(rel, root)?;
    let root = absolute(root);

    let mut item = dest.clone();
    loop {
        match fs::symlink_metadata(&item) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    return fail(
                        Code::SymlinkRefused,
                        format!("path crosses a symlink or junction: {}", item.display()),
                    );
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return fail(
                    Code::UnsafePath,
                    format!("could not inspect {}: {error}", item.display()),
                );
            }
        }

        if same_path(&item, &root) {
            break;
        }
        match item.parent() {
            // `assert_safe_relative` already proved dest is under root, so this
            // cannot loop forever - but a filesystem root parents itself.
            Some(parent) if parent != item.as_path() => item = parent.to_path_buf(),
            _ => return fail(Code::OutsideRoot, "walked past the filesystem root"),
        }
    }
    Ok(dest)
}

/// True when `child` is the same folder as, or nested inside, `parent`.
pub fn is_inside(child: &Path, parent: &Path) -> bool {
    let child = normalize(&absolute(child));
    let parent = normalize(&absolute(parent));
    // Component-wise, not textual: `.../ab` shares a string prefix with
    // `.../a` but is not inside it, which is the bug a `starts_with` on the
    // rendered path would introduce.
    child.starts_with(&parent[..])
}

/// Absolute form without requiring the path to exist. `canonicalize` would
/// resolve symlinks, which is the opposite of what the checks above need: they
/// must reason about the path as written.
fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Comparable component list. Windows paths are compared case-insensitively
/// because the filesystem treats them that way.
fn normalize(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|component| match component {
            Component::CurDir => None,
            other => Some(fold_case(&other.as_os_str().to_string_lossy())),
        })
        .collect()
}

fn fold_case(value: &str) -> String {
    if cfg!(windows) {
        value.to_lowercase()
    } else {
        value.to_owned()
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    normalize(a) == normalize(b)
}
