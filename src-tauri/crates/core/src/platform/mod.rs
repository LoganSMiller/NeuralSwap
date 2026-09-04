//! The few things that must ask Windows itself.
//!
//! Kept in one module so the rest of the core stays testable on any platform:
//! everything else takes paths as arguments, and only this decides what those
//! paths are on a real machine.

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

#[cfg(test)]
mod tests {
    use super::*;

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
