//! The record of what we installed, and where the originals went.
//!
//! Two jobs, both of which the scanner currently has to guess at:
//!
//! **Provenance.** [`crate::scan::folder::Provenance`] infers whether a runtime
//! was placed by hand by comparing it against the versions of its siblings.
//! That heuristic works - it correctly identified a hand-placed DLL on the
//! machine this was built on - but it is still an inference. A manifest turns
//! `OurInstall` into a fact: we wrote this file, at this version, and here is
//! the hash it had when we did.
//!
//! **Restore.** The user's original files are kept, permanently, outside the
//! journal, and this is what remembers where. An install that happened months
//! ago is still reversible, which is the difference between a tool somebody
//! trusts with a game they care about and one they try on something expendable.
//!
//! Stored one file per game rather than as a single index, so two games being
//! installed at once contend on nothing, and a corrupt record costs one game's
//! bookkeeping instead of all of it.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{fail, Code, Result};
use crate::fsx::atomic::{read_to_string_or_none, write_json_atomic};
use crate::install::plan::Route;
use crate::scan::folder::RuntimeKind;

pub const MANIFEST_VERSION: u32 = 1;

/// The file that was displaced, and where its only copy now lives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Replaced {
    pub sha256: String,
    pub size: u64,
    pub version: Option<String>,
    /// Absolute path in the backup store. Absolute because the store can sit
    /// on a different volume from the game.
    pub backup: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestFile {
    pub rel: String,
    pub kind: RuntimeKind,
    /// What we wrote, verified after writing.
    pub sha256: String,
    pub size: u64,
    pub version: Option<String>,
    /// `None` when the file did not exist before, and an uninstall therefore
    /// means deleting it rather than putting something back.
    pub replaced: Option<Replaced>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallManifest {
    pub version: u32,
    pub game_dir: PathBuf,
    pub route: Route,
    pub installed_at: i64,
    pub files: Vec<ManifestFile>,
}

impl InstallManifest {
    /// Relative paths we are responsible for, for feeding the planner's
    /// `managed` flag so it can tell "replacing our own file" from "replacing
    /// something the user or the game put there".
    pub fn managed_rels(&self) -> Vec<String> {
        self.files.iter().map(|file| file.rel.clone()).collect()
    }
}

/// A stable file name for a game directory.
///
/// Hashed rather than sanitised: a game path contains colons, separators and
/// arbitrary Unicode, and every scheme for flattening that into a file name
/// either collides or produces something unreadable anyway. Case-folded first,
/// because NTFS considers `D:\Games` and `d:\games` the same folder and so
/// must this.
pub fn key_for(game_dir: &Path) -> String {
    let folded = game_dir.to_string_lossy().replace('\\', "/").to_lowercase();
    let digest = crate::hash::hash_bytes(folded.as_bytes());
    digest.get(..16).unwrap_or(&digest).to_owned()
}

pub fn path_for(manifest_root: &Path, game_dir: &Path) -> PathBuf {
    manifest_root.join(format!("{}.json", key_for(game_dir)))
}

/// Read a game's manifest. `None` when nothing has been installed there.
///
/// A manifest that cannot be parsed is an error rather than a `None`: silently
/// treating a damaged record as "nothing installed" would strand the user's
/// original files in the backup store with nothing pointing at them.
pub fn load(manifest_root: &Path, game_dir: &Path) -> Result<Option<InstallManifest>> {
    let path = path_for(manifest_root, game_dir);
    let Some(text) = read_to_string_or_none(&path)? else {
        return Ok(None);
    };
    let parsed: InstallManifest = serde_json::from_str(&text).map_err(|error| {
        crate::Error::new(
            Code::StateCorrupt,
            format!("could not parse {}: {error}", path.display()),
        )
    })?;
    if parsed.version > MANIFEST_VERSION {
        return fail(
            Code::StateVersionAhead,
            format!(
                "{} was written by a newer build (version {})",
                path.display(),
                parsed.version
            ),
        );
    }
    Ok(Some(parsed))
}

pub fn save(manifest_root: &Path, manifest: &InstallManifest) -> Result<()> {
    write_json_atomic(&path_for(manifest_root, &manifest.game_dir), manifest)
}

/// Forget a game's install record. Does not touch the backup store - the
/// caller decides whether the originals have been put back first.
pub fn remove(manifest_root: &Path, game_dir: &Path) -> Result<()> {
    let path = path_for(manifest_root, game_dir);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => fail(
            Code::StateUnwritable,
            format!("could not remove {}: {error}", path.display()),
        ),
    }
}

/// What has happened to a file since we installed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FileStatus {
    /// Still exactly the bytes we wrote.
    Intact,
    /// Present, but no longer what we wrote. A game update replaced it, or
    /// another tool did, or the user did.
    Changed,
    /// Gone entirely.
    Missing,
    /// There, but we could not read it to find out.
    Unreadable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileReport {
    pub rel: String,
    pub status: FileStatus,
    /// Present for a `Changed` file, so the UI can say what it is now.
    pub found_sha256: Option<String>,
    /// Whether the original is still available to restore.
    pub restorable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Integrity {
    pub files: Vec<FileReport>,
    /// True when every recorded file is exactly as installed.
    pub intact: bool,
}

/// Check an install against what was recorded.
///
/// This is the answer to "is what I installed still there?", and it is a real
/// question rather than a rhetorical one: a game update overwrites the very
/// files a swap targets, which is why an upscaler swap silently reverts after
/// a patch. Being able to say so plainly - and name which files - is worth
/// more than re-running the install and hoping.
pub fn verify(manifest: &InstallManifest) -> Integrity {
    let mut files = Vec::with_capacity(manifest.files.len());
    for file in &manifest.files {
        let target = manifest.game_dir.join(file.rel.replace('\\', "/"));
        let restorable = file
            .replaced
            .as_ref()
            .is_some_and(|original| original.backup.is_file());

        // Hashed once. A runtime DLL is tens of megabytes, and reading it a
        // second time to report what it turned out to be would double the cost
        // of verifying an install for nothing.
        let (status, found_sha256) = if !target.exists() {
            (FileStatus::Missing, None)
        } else {
            match crate::hash::hash_file(&target) {
                Ok(found) if crate::hash::matches(&found, &file.sha256) => {
                    (FileStatus::Intact, None)
                }
                Ok(found) => (FileStatus::Changed, Some(found)),
                Err(_) => (FileStatus::Unreadable, None),
            }
        };

        files.push(FileReport {
            rel: file.rel.clone(),
            status,
            found_sha256,
            restorable,
        });
    }

    let intact = files
        .iter()
        .all(|report| report.status == FileStatus::Intact);
    Integrity { files, intact }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(game: &Path, files: Vec<ManifestFile>) -> InstallManifest {
        InstallManifest {
            version: MANIFEST_VERSION,
            game_dir: game.to_path_buf(),
            route: Route::NativeDll,
            installed_at: 1_700_000_000,
            files,
        }
    }

    fn file(rel: &str, sha: &str) -> ManifestFile {
        ManifestFile {
            rel: rel.to_owned(),
            kind: RuntimeKind::Dlss,
            sha256: sha.to_owned(),
            size: 4,
            version: Some("310.8.0.0".to_owned()),
            replaced: None,
        }
    }

    #[test]
    fn a_manifest_round_trips() {
        let root = tempfile::tempdir().expect("tempdir");
        let store = root.path().join("installs");
        let game = root.path().join("game");

        assert!(load(&store, &game).expect("load").is_none());

        let written = manifest(
            &game,
            vec![file(
                "bin/x64/nvngx_dlss.dll",
                &crate::hash::hash_bytes(b"ours"),
            )],
        );
        save(&store, &written).expect("save");
        assert_eq!(load(&store, &game).expect("load"), Some(written));

        remove(&store, &game).expect("remove");
        assert!(load(&store, &game).expect("load").is_none());
    }

    #[test]
    fn the_key_folds_case_and_separators_the_way_ntfs_does() {
        assert_eq!(
            key_for(Path::new("D:\\Games\\Cyberpunk 2077")),
            key_for(Path::new("d:/games/cyberpunk 2077"))
        );
        assert_ne!(
            key_for(Path::new("D:\\Games\\One")),
            key_for(Path::new("D:\\Games\\Two"))
        );
    }

    #[test]
    fn a_damaged_manifest_is_an_error_not_an_empty_one() {
        // Reporting "nothing installed" would strand the user's original files
        // in the backup store with nothing pointing at them.
        let root = tempfile::tempdir().expect("tempdir");
        let store = root.path().join("installs");
        let game = root.path().join("game");
        std::fs::create_dir_all(&store).expect("store");
        std::fs::write(path_for(&store, &game), b"{ not json").expect("write");

        assert_eq!(
            load(&store, &game).err().map(|error| error.code),
            Some(Code::StateCorrupt)
        );
    }

    #[test]
    fn a_manifest_from_a_newer_build_is_refused() {
        let root = tempfile::tempdir().expect("tempdir");
        let store = root.path().join("installs");
        let game = root.path().join("game");
        let mut future = manifest(&game, vec![]);
        future.version = MANIFEST_VERSION + 1;
        save(&store, &future).expect("save");

        assert_eq!(
            load(&store, &game).err().map(|error| error.code),
            Some(Code::StateVersionAhead)
        );
    }

    #[test]
    fn verification_reports_each_file_as_it_finds_it() {
        let root = tempfile::tempdir().expect("tempdir");
        let game = root.path().join("game");
        std::fs::create_dir_all(game.join("bin/x64")).expect("dirs");

        std::fs::write(game.join("bin/x64/intact.dll"), b"ours").expect("write");
        std::fs::write(game.join("bin/x64/changed.dll"), b"a game update").expect("write");
        // `missing.dll` is deliberately not created.

        let ours = crate::hash::hash_bytes(b"ours");
        let report = verify(&manifest(
            &game,
            vec![
                file("bin/x64/intact.dll", &ours),
                file("bin/x64/changed.dll", &ours),
                file("bin/x64/missing.dll", &ours),
            ],
        ));

        assert!(!report.intact);
        assert_eq!(report.files[0].status, FileStatus::Intact);
        assert_eq!(report.files[1].status, FileStatus::Changed);
        assert_eq!(report.files[2].status, FileStatus::Missing);
        // A changed file says what it is now, so the UI can be specific.
        assert_eq!(
            report.files[1].found_sha256.as_deref(),
            Some(crate::hash::hash_bytes(b"a game update").as_str())
        );
        assert!(report.files[0].found_sha256.is_none());
    }

    #[test]
    fn an_install_that_is_still_exactly_as_written_reports_intact() {
        let root = tempfile::tempdir().expect("tempdir");
        let game = root.path().join("game");
        std::fs::create_dir_all(&game).expect("dirs");
        std::fs::write(game.join("nvngx_dlss.dll"), b"ours").expect("write");

        let report = verify(&manifest(
            &game,
            vec![file("nvngx_dlss.dll", &crate::hash::hash_bytes(b"ours"))],
        ));
        assert!(report.intact);
    }

    #[test]
    fn restorability_follows_whether_the_backup_is_still_there() {
        let root = tempfile::tempdir().expect("tempdir");
        let game = root.path().join("game");
        let backups = root.path().join("backups");
        std::fs::create_dir_all(&game).expect("dirs");
        std::fs::create_dir_all(&backups).expect("dirs");
        let backup = backups.join("0000.bin");
        std::fs::write(&backup, b"the original").expect("write");
        std::fs::write(game.join("a.dll"), b"ours").expect("write");

        let mut entry = file("a.dll", &crate::hash::hash_bytes(b"ours"));
        entry.replaced = Some(Replaced {
            sha256: crate::hash::hash_bytes(b"the original"),
            size: 12,
            version: Some("310.1.0.0".to_owned()),
            backup: backup.clone(),
        });
        let record = manifest(&game, vec![entry]);

        assert!(verify(&record).files[0].restorable);
        std::fs::remove_file(&backup).expect("remove backup");
        assert!(!verify(&record).files[0].restorable);
    }

    #[test]
    fn managed_paths_come_back_for_the_planner() {
        let game = Path::new("D:\\Games\\Example");
        let record = manifest(
            game,
            vec![file("bin/x64/a.dll", "x"), file("bin/x64/b.dll", "y")],
        );
        assert_eq!(
            record.managed_rels(),
            vec!["bin/x64/a.dll".to_owned(), "bin/x64/b.dll".to_owned()]
        );
    }
}
