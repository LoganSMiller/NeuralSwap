//! Reading the two sides a plan compares.
//!
//! The planner is pure, which means somebody has to hand it facts: what the
//! package offers, and what is already in the folder. Both sides are described
//! the same way - name, kind, version, size, hash - so that the comparison is
//! between like and like.
//!
//! Hashing happens here rather than during a folder scan, and that is a
//! deliberate split. A scan walks tens of thousands of files and has to stay
//! in the tens of milliseconds; hashing every runtime it passes would ruin
//! that for information nobody has asked for yet. Planning an install looks at
//! a handful of files in one directory, where reading them properly costs
//! nothing worth measuring.

use std::path::Path;

use crate::error::{fail, Code, Result};
use crate::install::plan::{PackageFile, PresentFile};
use crate::scan::folder::classify_runtime;

/// The largest file we will treat as a runtime DLL.
///
/// A guard against being pointed at a folder containing a disc image and
/// hashing it. Real runtimes are tens of megabytes; nothing legitimate here is
/// close to this.
const MAX_RUNTIME_BYTES: u64 = 512 * 1024 * 1024;

/// What a package folder offers.
///
/// Only files that classify as a runtime are returned - a package downloaded
/// from anywhere will also contain a readme, a licence and possibly an
/// installer, and none of those belong in a game folder. Nested directories
/// are ignored rather than walked, because the plan writes into one directory
/// and a package that needs a tree is a package we do not understand.
pub fn read_package(dir: &Path) -> Result<Vec<PackageFile>> {
    let entries = std::fs::read_dir(dir).map_err(|error| {
        crate::Error::new(
            Code::PackageInvalid,
            format!("could not read {}: {error}", dir.display()),
        )
    })?;

    let mut files = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(kind) = classify_runtime(name) else {
            continue;
        };
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        if meta.len() > MAX_RUNTIME_BYTES {
            return fail(
                Code::PackageInvalid,
                format!("{name} is far too large to be a runtime DLL"),
            );
        }

        files.push(PackageFile {
            name: name.to_owned(),
            kind,
            version: version_of(&path),
            size: meta.len(),
            sha256: crate::hash::hash_file(&path)?,
        });
    }

    if files.is_empty() {
        return fail(
            Code::PackageInvalid,
            format!(
                "no runtime DLLs in {} - expected files like nvngx_dlss.dll or sl.dlss.dll",
                dir.display()
            ),
        );
    }
    // Sorted so a plan built from the same folder twice is the same plan.
    files.sort_by_key(|file| file.name.to_lowercase());
    Ok(files)
}

/// The runtime files already in one directory of a game.
///
/// `managed` is the set of relative paths our own install manifest claims, so
/// the planner can tell "replacing our own file" from "replacing something the
/// game or the user put there" - the distinction behind the
/// `replacesUnmanagedFile` warning.
///
/// A directory that does not exist is not an error: it is a folder the install
/// will create, and it contains nothing.
pub fn read_present(
    game_dir: &Path,
    install_dir: &str,
    managed: &[String],
) -> Result<Vec<PresentFile>> {
    let folder = if install_dir.is_empty() {
        game_dir.to_path_buf()
    } else {
        // Resolved through the safety check rather than joined blindly: the
        // install directory reaches us from a scan result or a manifest, and
        // neither is a reason to skip the check that a junction cannot
        // redirect us out of the game folder.
        crate::fsx::paths::safe_path(game_dir, install_dir)?
    };

    let entries = match std::fs::read_dir(&folder) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return fail(
                Code::UnsafePath,
                format!("could not read {}: {error}", folder.display()),
            )
        }
    };

    let managed_keys: Vec<String> = managed
        .iter()
        .map(|rel| rel.replace('\\', "/").to_lowercase())
        .collect();

    let mut files = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(kind) = classify_runtime(name) else {
            continue;
        };
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() || meta.len() > MAX_RUNTIME_BYTES {
            continue;
        }

        let rel = if install_dir.is_empty() {
            name.to_owned()
        } else {
            format!(
                "{}/{name}",
                install_dir.replace('\\', "/").trim_end_matches('/')
            )
        };
        let key = rel.to_lowercase();

        files.push(PresentFile {
            managed: managed_keys.contains(&key),
            rel,
            kind,
            version: version_of(&path),
            size: meta.len(),
            sha256: crate::hash::hash_file(&path)?,
        });
    }

    files.sort_by_key(|file| file.rel.to_lowercase());
    Ok(files)
}

/// A DLL with no version resource is normal, not an error - the planner treats
/// an unknown version as "cannot claim this is an upgrade".
fn version_of(path: &Path) -> Option<String> {
    crate::pe::PeFile::with(path, |pe| pe.file_version(), None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::folder::RuntimeKind;

    fn scratch() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[test]
    fn a_package_lists_only_its_runtime_files() {
        let dir = scratch();
        std::fs::write(dir.path().join("nvngx_dlss.dll"), b"upscaler").expect("write");
        std::fs::write(dir.path().join("sl.dlss_g.dll"), b"frame gen").expect("write");
        // The things a downloaded package also contains, none of which belong
        // anywhere near a game folder.
        std::fs::write(dir.path().join("README.md"), b"instructions").expect("write");
        std::fs::write(dir.path().join("LICENCE.txt"), b"terms").expect("write");
        std::fs::write(dir.path().join("setup.exe"), b"an installer").expect("write");
        std::fs::write(dir.path().join("d3d12.dll"), b"not a runtime we manage").expect("write");
        std::fs::create_dir(dir.path().join("nested")).expect("dir");

        let package = read_package(dir.path()).expect("read");
        assert_eq!(
            package
                .iter()
                .map(|file| file.name.as_str())
                .collect::<Vec<_>>(),
            vec!["nvngx_dlss.dll", "sl.dlss_g.dll"]
        );
        assert_eq!(package[0].kind, RuntimeKind::Dlss);
        assert_eq!(package[1].kind, RuntimeKind::Streamline);
        assert_eq!(package[0].sha256, crate::hash::hash_bytes(b"upscaler"));
        assert_eq!(package[0].size, 8);
        // No version resource in a stub file, which is a normal outcome.
        assert_eq!(package[0].version, None);
    }

    #[test]
    fn a_folder_with_no_runtimes_says_so_usefully() {
        let dir = scratch();
        std::fs::write(dir.path().join("README.md"), b"nothing here").expect("write");
        let error = read_package(dir.path()).expect_err("should refuse");
        assert_eq!(error.code, Code::PackageInvalid);
        // The message names what was expected, because "invalid package" alone
        // tells a user nothing about what to do next.
        assert!(error.detail.contains("nvngx_dlss.dll"));
    }

    #[test]
    fn a_missing_package_folder_is_refused_rather_than_empty() {
        let dir = scratch();
        let error = read_package(&dir.path().join("not-there")).expect_err("should refuse");
        assert_eq!(error.code, Code::PackageInvalid);
    }

    #[test]
    fn an_absurdly_large_file_is_refused_rather_than_hashed() {
        // Guards against being pointed at a folder holding a disc image that
        // happens to be named like a runtime.
        let dir = scratch();
        let path = dir.path().join("nvngx_dlss.dll");
        let file = std::fs::File::create(&path).expect("create");
        file.set_len(MAX_RUNTIME_BYTES + 1).expect("size it");
        drop(file);

        let error = read_package(dir.path()).expect_err("should refuse");
        assert_eq!(error.code, Code::PackageInvalid);
        assert!(error.detail.contains("too large"));
    }

    #[test]
    fn present_files_are_read_from_the_install_directory_only() {
        let dir = scratch();
        let game = dir.path();
        std::fs::create_dir_all(game.join("bin/x64")).expect("dirs");
        std::fs::write(game.join("bin/x64/nvngx_dlss.dll"), b"in the right place").expect("write");
        // The stray copy in the game root that the loader never looks at.
        std::fs::write(game.join("nvngx_dlss.dll"), b"stray").expect("write");

        let present = read_present(game, "bin/x64", &[]).expect("read");
        assert_eq!(present.len(), 1);
        assert_eq!(present[0].rel, "bin/x64/nvngx_dlss.dll");
        assert_eq!(
            present[0].sha256,
            crate::hash::hash_bytes(b"in the right place")
        );
        assert!(!present[0].managed);
    }

    #[test]
    fn an_install_directory_that_does_not_exist_yet_is_empty_not_an_error() {
        let dir = scratch();
        assert!(read_present(dir.path(), "bin/x64", &[])
            .expect("read")
            .is_empty());
    }

    #[test]
    fn the_game_root_is_the_install_directory_when_it_is_empty() {
        let dir = scratch();
        std::fs::write(dir.path().join("nvngx_dlss.dll"), b"at the root").expect("write");
        let present = read_present(dir.path(), "", &[]).expect("read");
        assert_eq!(present.len(), 1);
        assert_eq!(present[0].rel, "nvngx_dlss.dll");
    }

    #[test]
    fn our_own_files_are_flagged_as_managed() {
        let dir = scratch();
        let game = dir.path();
        std::fs::create_dir_all(game.join("bin/x64")).expect("dirs");
        std::fs::write(game.join("bin/x64/nvngx_dlss.dll"), b"ours").expect("write");
        std::fs::write(game.join("bin/x64/nvngx_dlssg.dll"), b"theirs").expect("write");

        // Recorded with the other separator and a different case, as a
        // manifest written on another run might have it.
        let managed = vec!["bin\\x64\\NVNGX_DLSS.DLL".to_owned()];
        let present = read_present(game, "bin/x64", &managed).expect("read");

        let managed_for = |rel: &str| {
            present
                .iter()
                .find(|file| file.rel == rel)
                .map(|file| file.managed)
                .expect("file present")
        };
        assert!(managed_for("bin/x64/nvngx_dlss.dll"));
        assert!(!managed_for("bin/x64/nvngx_dlssg.dll"));
    }

    #[test]
    fn an_install_directory_reached_through_a_junction_is_refused() {
        let dir = scratch();
        let game = dir.path();
        let elsewhere = game.join("real");
        std::fs::create_dir_all(&elsewhere).expect("dirs");

        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_dir(&elsewhere, game.join("linked")).is_ok();
        #[cfg(not(windows))]
        let made = std::os::unix::fs::symlink(&elsewhere, game.join("linked")).is_ok();
        if !made {
            return;
        }

        let error = read_present(game, "linked", &[]).expect_err("should refuse");
        assert_eq!(error.code, Code::SymlinkRefused);
    }

    #[test]
    fn a_real_runtime_on_this_machine_reports_its_version() {
        // Against a real signed DLL rather than a stub, so the version reader
        // is exercised on the shape it will actually meet. Skipped where the
        // file is not present.
        let candidates = [
            "C:\\Windows\\System32\\nvngx_dlss.dll",
            "C:\\Windows\\System32\\kernel32.dll",
        ];
        let Some(found) = candidates.iter().map(Path::new).find(|path| path.is_file()) else {
            return;
        };
        let version = version_of(found);
        assert!(
            version.is_some(),
            "{} should carry a version resource",
            found.display()
        );
    }
}
