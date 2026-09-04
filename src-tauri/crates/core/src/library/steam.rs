//! Finding Steam's libraries and what is installed in them.
//!
//! Steam is authoritative about its own installs, so this reads its records
//! rather than guessing at folder names: `libraryfolders.vdf` lists every
//! library root (people move games to a second drive constantly), and one
//! `appmanifest_<id>.acf` per installed app gives the id, the display name and
//! the folder under `steamapps/common`.
//!
//! The parsing is separated from the filesystem work so it can be tested
//! against real manifest text without a Steam install present.

use std::path::{Path, PathBuf};

use super::vdf;
use super::{Game, Source};

/// Library roots named by a `libraryfolders.vdf`, plus the Steam root itself.
///
/// Modern Steam writes an object per library with a `path`; much older
/// versions wrote `"1" "D:\\SteamLibrary"` directly. Both appear in the wild
/// on machines that have been upgraded for years, so both are read.
pub fn libraries_from_vdf(document: &str, steam_root: &Path) -> Vec<PathBuf> {
    let mut out = vec![steam_root.to_path_buf()];
    let root = vdf::parse(document);

    let folders = root
        .get("libraryfolders")
        .or_else(|| root.get("LibraryFolders"));
    let Some(folders) = folders else { return out };

    for (key, value) in folders.entries() {
        // Keys are library indices; anything else is metadata.
        if !key.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let path = match value {
            vdf::Value::Text(text) => Some(text.clone()),
            vdf::Value::Object(_) => value
                .get("path")
                .and_then(|p| p.as_text())
                .map(str::to_owned),
        };
        if let Some(path) = path.filter(|p| !p.is_empty()) {
            let candidate = PathBuf::from(path);
            if !out.iter().any(|existing| same_path(existing, &candidate)) {
                out.push(candidate);
            }
        }
    }
    out
}

fn same_path(a: &Path, b: &Path) -> bool {
    a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
}

/// Steam entries that install like games but are not.
///
/// These sit in `steamapps/common` beside real games and have ordinary
/// manifests, so nothing about their shape distinguishes them. Offering
/// "Steamworks Common Redistributables" as a game to inject into is at best
/// confusing and at worst somebody patching a shared runtime every other game
/// on the machine depends on.
///
/// Matched by id where the id is stable, and by name for the families whose
/// ids multiply with every release - there is a new Proton app id every year.
const NOT_A_GAME_APP_IDS: &[&str] = &[
    "228980",  // Steamworks Common Redistributables
    "1070560", // Steam Linux Runtime 1.0
    "1391110", // Steam Linux Runtime 2.0
    "1628350", // Steam Linux Runtime 3.0
    "1493710", // Proton Experimental
];

const NOT_A_GAME_NAME_PARTS: &[&str] = &[
    "steamworks common redistributables",
    "steam linux runtime",
    "proton ",
    "proton experimental",
    "proton hotfix",
];

fn is_not_a_game(app_id: Option<&str>, name: &str) -> bool {
    if app_id.is_some_and(|id| NOT_A_GAME_APP_IDS.contains(&id)) {
        return true;
    }
    let lower = name.to_lowercase();
    NOT_A_GAME_NAME_PARTS
        .iter()
        .any(|part| lower == part.trim() || lower.starts_with(part))
}

/// A game from one `appmanifest_*.acf`, given the library it lives in.
///
/// Returns `None` for an app that is not actually installed: Steam keeps a
/// manifest for a download that was queued and never finished, and offering
/// that as a game produces a folder that is empty or missing.
pub fn game_from_manifest(document: &str, library: &Path) -> Option<Game> {
    let root = vdf::parse(document);
    let state = root.get("AppState")?;

    let install_dir = state.get("installdir")?.as_text()?;
    if install_dir.is_empty() {
        return None;
    }
    let app_id = state
        .get("appid")
        .and_then(|v| v.as_text())
        .map(str::to_owned);
    let name = state
        .get("name")
        .and_then(|v| v.as_text())
        .filter(|n| !n.is_empty())
        .unwrap_or(install_dir)
        .to_owned();

    // StateFlags 4 is "fully installed". Steam ORs flags together, so a value
    // with the bit set alongside others (an update pending, say) still means
    // the files are on disk.
    let fully_installed = state
        .get("StateFlags")
        .and_then(|v| v.as_text())
        .and_then(|text| text.parse::<u32>().ok())
        .is_none_or(|flags| flags & 4 != 0);
    if !fully_installed {
        return None;
    }
    if is_not_a_game(app_id.as_deref(), &name) {
        return None;
    }

    Some(Game {
        name,
        dir: library.join("steamapps").join("common").join(install_dir),
        source: Source::Steam,
        app_id,
    })
}

/// Every installed Steam game, given the Steam root.
pub fn discover(steam_root: &Path) -> Vec<Game> {
    let vdf_path = steam_root.join("steamapps").join("libraryfolders.vdf");
    let document = std::fs::read_to_string(&vdf_path).unwrap_or_default();
    let libraries = libraries_from_vdf(&document, steam_root);

    let mut games = Vec::new();
    for library in libraries {
        let steamapps = library.join("steamapps");
        let Ok(entries) = std::fs::read_dir(&steamapps) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let lower = name.to_lowercase();
            if !lower.starts_with("appmanifest_") || !lower.ends_with(".acf") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(entry.path()) else {
                continue;
            };
            if let Some(game) = game_from_manifest(&text, &library) {
                // A manifest can outlive the folder it names.
                if game.dir.is_dir() {
                    games.push(game);
                }
            }
        }
    }
    games
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_steam_root_is_always_a_library() {
        let root = Path::new(r"C:\Steam");
        let found = libraries_from_vdf("", root);
        assert_eq!(found, vec![PathBuf::from(r"C:\Steam")]);
    }

    #[test]
    fn modern_library_entries_are_read() {
        let document = r#"
"libraryfolders"
{
	"0"
	{
		"path"		"C:\\Program Files (x86)\\Steam"
	}
	"1"
	{
		"path"		"D:\\SteamLibrary"
	}
	"contentstatsid"		"12345"
}
"#;
        let found = libraries_from_vdf(document, Path::new(r"C:\Program Files (x86)\Steam"));
        assert_eq!(
            found,
            vec![
                PathBuf::from(r"C:\Program Files (x86)\Steam"),
                PathBuf::from(r"D:\SteamLibrary"),
            ],
            "the Steam root must not be duplicated, and metadata keys are not libraries"
        );
    }

    #[test]
    fn the_older_flat_layout_is_still_read() {
        // Machines upgraded over years still carry this shape.
        let document = r#"
"LibraryFolders"
{
	"TimeNextStatsReport"		"1700000000"
	"1"		"E:\\Games\\Steam"
}
"#;
        let found = libraries_from_vdf(document, Path::new(r"C:\Steam"));
        assert!(
            found.contains(&PathBuf::from(r"E:\Games\Steam")),
            "{found:?}"
        );
        // A numeric-looking metadata key must not be mistaken for a path.
        assert_eq!(found.len(), 2);
    }

    /// A root that is absolute on whichever platform the tests run on.
    ///
    /// Real manifests only ever carry Windows paths, but asserting on a
    /// rendered `D:\...` string makes the test fail on Linux over separator
    /// style rather than over behaviour. The invariant worth checking is the
    /// layout - library, then `steamapps/common`, then the install folder.
    fn library_root() -> PathBuf {
        PathBuf::from(if cfg!(windows) {
            r"D:\SteamLibrary"
        } else {
            "/steamlibrary"
        })
    }

    #[test]
    fn an_installed_game_becomes_a_library_entry() {
        let document = r#"
"AppState"
{
	"appid"		"1091500"
	"name"		"Cyberpunk 2077"
	"installdir"		"Cyberpunk 2077"
	"StateFlags"		"4"
}
"#;
        let library = library_root();
        let game = game_from_manifest(document, &library).expect("game");
        assert_eq!(game.name, "Cyberpunk 2077");
        assert_eq!(game.app_id.as_deref(), Some("1091500"));
        assert_eq!(
            game.dir,
            library
                .join("steamapps")
                .join("common")
                .join("Cyberpunk 2077")
        );
        assert_eq!(game.source, Source::Steam);
    }

    #[test]
    fn a_queued_but_uninstalled_app_is_skipped() {
        // StateFlags without bit 4 means the files are not on disk. Offering
        // it produces a folder that is empty or missing.
        let document = r#"
"AppState"
{
	"appid"		"12345"
	"name"		"Not Installed Yet"
	"installdir"		"NotInstalled"
	"StateFlags"		"1026"
}
"#;
        assert!(game_from_manifest(document, Path::new(r"D:\Lib")).is_none());
    }

    #[test]
    fn an_update_pending_game_is_still_installed() {
        // 4 | 2 = "installed, update queued": the files are there.
        let document = r#"
"AppState"
{
	"appid"		"12345"
	"name"		"Updating"
	"installdir"		"Updating"
	"StateFlags"		"6"
}
"#;
        assert!(game_from_manifest(document, Path::new(r"D:\Lib")).is_some());
    }

    #[test]
    fn a_manifest_without_state_flags_is_taken_as_installed() {
        // Older manifests omit the field; refusing them would lose real games.
        let document = r#""AppState" { "appid" "1" "name" "Old" "installdir" "Old" }"#;
        assert!(game_from_manifest(document, Path::new(r"D:\Lib")).is_some());
    }

    #[test]
    fn the_install_folder_is_the_fallback_name() {
        let document = r#""AppState" { "installdir" "SomeGame" "StateFlags" "4" }"#;
        let game = game_from_manifest(document, Path::new(r"D:\Lib")).expect("game");
        assert_eq!(game.name, "SomeGame");
        assert_eq!(game.app_id, None);
    }

    #[test]
    fn steam_tools_are_not_offered_as_games() {
        // These sit in steamapps/common with ordinary manifests. Injecting
        // into the shared redistributables would touch every other game.
        let redistributables = r#"
"AppState"
{
	"appid"		"228980"
	"name"		"Steamworks Common Redistributables"
	"installdir"		"Steamworks Shared"
	"StateFlags"		"4"
}
"#;
        assert!(game_from_manifest(redistributables, Path::new(r"D:\Lib")).is_none());

        // Proton and the Linux runtimes get a new app id every release, so
        // they are matched by name rather than by an id list that goes stale.
        for name in [
            "Proton 9.0",
            "Proton Experimental",
            "Proton Hotfix",
            "Steam Linux Runtime 3.0 (sniper)",
        ] {
            let document = format!(
                r#""AppState" {{ "appid" "999999" "name" "{name}" "installdir" "x" "StateFlags" "4" }}"#
            );
            assert!(
                game_from_manifest(&document, Path::new(r"D:\Lib")).is_none(),
                "{name} should not be offered as a game"
            );
        }
    }

    #[test]
    fn a_real_game_is_not_caught_by_the_tool_filter() {
        for name in [
            "Cyberpunk 2077",
            "Protonaut",
            "Ready or Not",
            "Steamworld Dig",
        ] {
            let document = format!(
                r#""AppState" {{ "appid" "1" "name" "{name}" "installdir" "x" "StateFlags" "4" }}"#
            );
            assert!(
                game_from_manifest(&document, Path::new(r"D:\Lib")).is_some(),
                "{name} should be kept"
            );
        }
    }

    #[test]
    fn a_manifest_with_no_install_folder_is_refused() {
        assert!(
            game_from_manifest(r#""AppState" { "appid" "1" }"#, Path::new(r"D:\Lib")).is_none()
        );
        assert!(game_from_manifest("garbage", Path::new(r"D:\Lib")).is_none());
    }
}
