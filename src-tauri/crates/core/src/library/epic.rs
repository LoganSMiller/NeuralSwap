//! Epic Games Launcher installs.
//!
//! Epic writes one JSON manifest per installed title into
//! `%ProgramData%\Epic\EpicGamesLauncher\Data\Manifests`, each naming the
//! install folder and the display name. Reading those is exact, where guessing
//! at folder names under an "Epic Games" directory is not - Epic lets a title
//! be installed anywhere, including another drive.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::{Game, Source};

/// Only the fields that matter. Epic's manifests carry a few dozen more, and
/// unknown fields are ignored rather than making the manifest unreadable.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Manifest {
    #[serde(default)]
    install_location: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    app_name: String,
    /// Epic marks a title that is present but not fully installed.
    ///
    /// Named explicitly: Epic follows Unreal's boolean convention with a
    /// lower-case `b` prefix, which `PascalCase` would render as
    /// `BIsIncompleteInstall` and never match - so every half-downloaded title
    /// would be listed as installed.
    #[serde(default, rename = "bIsIncompleteInstall")]
    b_is_incomplete_install: bool,
}

pub fn game_from_manifest(document: &str) -> Option<Game> {
    let manifest: Manifest = serde_json::from_str(document).ok()?;
    if manifest.install_location.is_empty() || manifest.b_is_incomplete_install {
        return None;
    }

    let dir = PathBuf::from(&manifest.install_location);
    let name = if manifest.display_name.is_empty() {
        dir.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| manifest.app_name.clone())
    } else {
        manifest.display_name.clone()
    };
    if name.is_empty() {
        return None;
    }

    Some(Game {
        name,
        dir,
        source: Source::Epic,
        app_id: (!manifest.app_name.is_empty()).then_some(manifest.app_name),
    })
}

/// Every installed Epic title, given the manifests folder.
pub fn discover(manifests: &Path) -> Vec<Game> {
    let Ok(entries) = std::fs::read_dir(manifests) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("item"))
        })
        .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
        .filter_map(|text| game_from_manifest(&text))
        .filter(|game| game.dir.is_dir())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_installed_title_becomes_a_library_entry() {
        let document = r#"{
            "InstallLocation": "D:\\Epic\\SomeGame",
            "DisplayName": "Some Game",
            "AppName": "abc123",
            "bIsIncompleteInstall": false
        }"#;
        let game = game_from_manifest(document).expect("game");
        assert_eq!(game.name, "Some Game");
        assert_eq!(game.dir, PathBuf::from(r"D:\Epic\SomeGame"));
        assert_eq!(game.app_id.as_deref(), Some("abc123"));
        assert_eq!(game.source, Source::Epic);
    }

    #[test]
    fn an_incomplete_install_is_skipped() {
        let document = r#"{
            "InstallLocation": "D:\\Epic\\Half",
            "DisplayName": "Half Downloaded",
            "bIsIncompleteInstall": true
        }"#;
        assert!(game_from_manifest(document).is_none());
    }

    #[test]
    fn the_folder_name_is_the_fallback_title() {
        // Built for the host platform: real manifests only ever carry Windows
        // paths, but `file_name` does not split on a backslash off Windows, so
        // hard-coding one would test the separator rather than the fallback.
        let location = if cfg!(windows) {
            r"D:\Epic\FolderName"
        } else {
            "/epic/FolderName"
        };
        let document = format!(
            r#"{{ "InstallLocation": {} }}"#,
            serde_json::to_string(location).expect("encode")
        );
        let game = game_from_manifest(&document).expect("game");
        assert_eq!(game.name, "FolderName");
        assert_eq!(game.app_id, None);
    }

    #[test]
    fn unknown_fields_do_not_make_a_manifest_unreadable() {
        // Epic adds fields between launcher versions; refusing the document
        // over one would lose the whole library on an update.
        let document = r#"{
            "InstallLocation": "D:\\Epic\\Game",
            "DisplayName": "Game",
            "SomethingNew": { "nested": [1, 2, 3] },
            "AnotherThing": 42
        }"#;
        assert!(game_from_manifest(document).is_some());
    }

    #[test]
    fn a_manifest_with_no_location_is_refused() {
        assert!(game_from_manifest(r#"{ "DisplayName": "No Location" }"#).is_none());
        assert!(game_from_manifest("not json at all").is_none());
        assert!(game_from_manifest("").is_none());
    }

    #[test]
    fn a_missing_manifests_folder_is_not_an_error() {
        assert!(discover(Path::new("no/such/place")).is_empty());
    }
}
