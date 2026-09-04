//! Xbox / Game Pass installs.
//!
//! Modern Game Pass titles install as flat files under a per-drive `XboxGames`
//! folder, one directory per game, with the playable content under `Content`
//! and a `MicrosoftGame.config` naming it. That is a far friendlier layout
//! than the older `WindowsApps` one, whose files are not readable at all
//! without taking ownership - which this will never do.

use std::path::{Path, PathBuf};

use super::{Game, Source};

/// Pull the display name out of a `MicrosoftGame.config`.
///
/// It is XML, but only two fields are wanted and neither is nested in anything
/// ambiguous, so this reads the attributes directly rather than pulling in an
/// XML parser for a file whose shape is fixed.
pub fn name_from_config(document: &str) -> Option<String> {
    for tag in ["ShellVisuals", "Identity"] {
        let Some(start) = document.find(&format!("<{tag}")) else {
            continue;
        };
        let rest = document.get(start..)?;
        let end = rest.find('>').unwrap_or(rest.len());
        let element = rest.get(..end)?;
        for attribute in ["DefaultDisplayName", "Name"] {
            if let Some(value) = attribute_value(element, attribute) {
                if !value.is_empty() {
                    return Some(value);
                }
            }
        }
    }
    None
}

fn attribute_value(element: &str, attribute: &str) -> Option<String> {
    let needle = format!("{attribute}=\"");
    let at = element.find(&needle)? + needle.len();
    let rest = element.get(at..)?;
    let end = rest.find('"')?;
    Some(rest.get(..end)?.trim().to_owned())
}

/// Games under one `XboxGames`-style root.
pub fn discover(root: &Path) -> Vec<Game> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut games = Vec::new();

    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let folder_name = entry.file_name().to_string_lossy().into_owned();
        // Game Pass keeps its own bookkeeping alongside the games.
        if folder_name.eq_ignore_ascii_case("GameSave") {
            continue;
        }

        // The config sits either in the game folder or under `Content`.
        let name = ["MicrosoftGame.config", "Content/MicrosoftGame.config"]
            .iter()
            .filter_map(|rel| std::fs::read_to_string(dir.join(rel)).ok())
            .find_map(|text| name_from_config(&text))
            .unwrap_or(folder_name);

        games.push(Game {
            name,
            dir,
            source: Source::Xbox,
            app_id: None,
        });
    }
    games
}

/// Roots to look in: an `XboxGames` folder at the top of every fixed drive.
pub fn default_roots(drives: &[PathBuf]) -> Vec<PathBuf> {
    drives
        .iter()
        .map(|drive| drive.join("XboxGames"))
        .filter(|path| path.is_dir())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_display_name_is_taken_from_shell_visuals() {
        let document = r#"<?xml version="1.0" encoding="utf-8"?>
<Game configVersion="1">
  <Identity Name="Microsoft.SomeGame" Publisher="CN=Microsoft" Version="1.0.0.0" />
  <ShellVisuals DefaultDisplayName="Halo: Campaign Evolved" PublisherDisplayName="Xbox" />
</Game>"#;
        assert_eq!(
            name_from_config(document).as_deref(),
            Some("Halo: Campaign Evolved")
        );
    }

    #[test]
    fn the_identity_name_is_the_fallback() {
        let document = r#"<Game><Identity Name="Microsoft.SomeGame" Version="1.0" /></Game>"#;
        assert_eq!(
            name_from_config(document).as_deref(),
            Some("Microsoft.SomeGame")
        );
    }

    #[test]
    fn a_config_with_no_usable_name_yields_none() {
        assert_eq!(name_from_config("<Game></Game>"), None);
        assert_eq!(name_from_config(""), None);
        // Present but empty is not a name.
        assert_eq!(
            name_from_config(r#"<ShellVisuals DefaultDisplayName="" />"#),
            None
        );
    }

    #[test]
    fn folders_are_listed_and_bookkeeping_is_skipped() {
        let scratch = tempfile::tempdir().expect("tempdir");
        let root = scratch.path();
        std::fs::create_dir_all(root.join("A Game")).expect("mkdir");
        std::fs::create_dir_all(root.join("GameSave")).expect("mkdir");
        std::fs::write(root.join("stray.txt"), b"not a game").expect("write");

        let games = discover(root);
        assert_eq!(games.len(), 1, "{games:?}");
        assert_eq!(games.first().map(|g| g.name.as_str()), Some("A Game"));
        assert_eq!(games.first().map(|g| g.source), Some(Source::Xbox));
    }

    #[test]
    fn a_config_under_content_names_the_game() {
        let scratch = tempfile::tempdir().expect("tempdir");
        let root = scratch.path();
        let game = root.join("SomeFolder");
        std::fs::create_dir_all(game.join("Content")).expect("mkdir");
        std::fs::write(
            game.join("Content").join("MicrosoftGame.config"),
            br#"<Game><ShellVisuals DefaultDisplayName="Proper Title" /></Game>"#,
        )
        .expect("write");

        let games = discover(root);
        assert_eq!(games.first().map(|g| g.name.as_str()), Some("Proper Title"));
    }

    #[test]
    fn a_missing_root_is_not_an_error() {
        assert!(discover(Path::new("no/such/place")).is_empty());
    }
}
