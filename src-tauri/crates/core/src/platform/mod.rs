//! The few things that must ask Windows itself.
//!
//! Kept in one module so the rest of the core stays testable on any platform:
//! everything else takes paths as arguments, and only this decides what those
//! paths are on a real machine.

pub mod gpu;
use std::path::PathBuf;

use crate::library::{xbox, Roots};

/// Fixed drives worth searching, as `C:\`, `D:\` and so on.
///
/// Probed rather than enumerated through an API: checking whether `X:\` exists
/// is cheap, needs no dependency, and naturally skips drives that are not
/// mounted. Removable and network drives answer too, which is acceptable -
/// the only use is looking for a folder that is either there or not.
pub fn fixed_drives() -> Vec<PathBuf> {
    if !cfg!(windows) {
        return vec![PathBuf::from("/")];
    }
    (b'C'..=b'Z')
        .map(|letter| PathBuf::from(format!("{}:\\", char::from(letter))))
        .filter(|drive| drive.is_dir())
        .collect()
}

/// Where Steam is installed.
///
/// The registry is authoritative; the common locations are a fallback for a
/// machine where the key is missing or unreadable.
pub fn steam_root() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        for (hive, subkey, value) in [
            (
                windows_registry::CURRENT_USER,
                r"Software\Valve\Steam",
                "SteamPath",
            ),
            (
                windows_registry::LOCAL_MACHINE,
                r"SOFTWARE\WOW6432Node\Valve\Steam",
                "InstallPath",
            ),
        ] {
            if let Ok(key) = hive.open(subkey) {
                if let Ok(path) = key.get_string(value) {
                    let path = PathBuf::from(path.replace('/', "\\"));
                    if path.is_dir() {
                        return Some(path);
                    }
                }
            }
        }
    }

    [
        r"C:\Program Files (x86)\Steam",
        r"C:\Program Files\Steam",
        r"C:\Steam",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|path| path.is_dir())
}

/// Epic's manifests folder, under ProgramData.
pub fn epic_manifests() -> Option<PathBuf> {
    let program_data = std::env::var("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(r"C:\ProgramData"));
    let path = program_data
        .join("Epic")
        .join("EpicGamesLauncher")
        .join("Data")
        .join("Manifests");
    path.is_dir().then_some(path)
}

/// Everywhere worth looking on this machine.
pub fn roots() -> Roots {
    Roots {
        steam: steam_root(),
        epic_manifests: epic_manifests(),
        xbox: xbox::default_roots(&fixed_drives()),
    }
}

/// Bytes still available to this user on the volume holding `path`.
///
/// "Available to the caller" rather than total free, because a volume with a
/// disk quota can have free space this user cannot have. `None` when the
/// question cannot be answered - an unmounted drive, a path that no longer
/// exists, or a platform without the call - and a caller must treat that as
/// "unknown" rather than as zero. Refusing an install because we could not
/// measure a disk would be worse than attempting it and rolling back.
///
/// The workspace warns on `unsafe_code`, and that stays: this is the only
/// place in the crate that needs it, and the lint should keep firing anywhere
/// else. There is no safe route to this number - it is a Win32 call with
/// out-parameters, and the alternative is a much larger dependency wrapping
/// the same call. Allowed here, at one function, rather than crate-wide.
#[allow(unsafe_code)]
pub fn free_space(path: &std::path::Path) -> Option<u64> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

        // The call wants a directory. A file path answers for its parent, so
        // walk up to something that exists before asking.
        let mut probe = path;
        loop {
            if probe.is_dir() {
                break;
            }
            probe = probe.parent()?;
        }

        let wide: Vec<u16> = probe
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut available: u64 = 0;
        // SAFETY: `wide` is NUL-terminated and outlives the call; the output
        // pointer is a live local. The two totals we do not need are passed as
        // null, which the API documents as permitted.
        let ok = unsafe {
            GetDiskFreeSpaceExW(
                wide.as_ptr(),
                &raw mut available,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return None;
        }
        Some(available)
    }
    #[cfg(not(windows))]
    {
        // Not wired up off Windows: the tests that care assert on the
        // "unknown" branch, which is the same branch a quota-limited or
        // disconnected volume takes on Windows.
        let _ = path;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_space_answers_for_a_real_directory_and_a_file_inside_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let Some(bytes) = free_space(dir.path()) else {
            // Off Windows there is no implementation, which is a documented
            // outcome rather than a failure. On Windows, reaching here means
            // the call itself failed on an ordinary temp directory, which is
            // a real problem worth failing the test over.
            #[cfg(windows)]
            panic!("Windows should be able to measure a temp directory");
            #[cfg(not(windows))]
            return;
        };
        assert!(bytes > 0, "a writable temp directory with no space?");

        // A path that does not exist yet still answers, via its parent - which
        // is the case that matters, because the file has not been written.
        let unwritten = dir.path().join("not-there-yet.dll");
        assert!(free_space(&unwritten).is_some());
    }

    #[test]
    fn free_space_is_unknown_rather_than_zero_for_nonsense() {
        // A drive letter that cannot be mounted. Answering zero here would
        // make a caller refuse an install for lack of space.
        assert_eq!(
            free_space(std::path::Path::new("\\\\?\\GLOBALROOT\\nope")),
            None
        );
    }

    #[test]
    fn at_least_one_drive_is_found() {
        let drives = fixed_drives();
        assert!(!drives.is_empty(), "no drives at all?");
        for drive in &drives {
            assert!(
                drive.is_dir(),
                "{} was reported but is not a directory",
                drive.display()
            );
        }
    }

    #[test]
    fn discovering_roots_never_panics() {
        // Whatever this machine has or has not got installed, asking must be
        // safe: this runs at startup before there is a window to report into.
        let found = roots();
        if let Some(steam) = &found.steam {
            assert!(steam.is_dir());
        }
        for root in &found.xbox {
            assert!(root.is_dir());
        }
    }
}
