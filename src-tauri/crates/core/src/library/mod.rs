//! Finding the games already installed on the machine.
//!
//! Each storefront is asked about its own installs rather than guessed at, and
//! the results are merged and de-duplicated. A game reachable through two
//! stores is one entry, not two.

pub mod epic;
pub mod steam;
pub mod vdf;
pub mod xbox;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Source {
    Steam,
    Epic,
    Xbox,
    /// A folder the user pointed at themselves.
    Manual,
}

impl Source {
    pub const fn label(self) -> &'static str {
        match self {
            Source::Steam => "Steam",
            Source::Epic => "Epic Games",
            Source::Xbox => "Xbox",
            Source::Manual => "Added by hand",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Game {
    pub name: String,
    pub dir: PathBuf,
    pub source: Source,
    /// The store's own identifier, where it has one. Steam's app id is what
    /// makes cover art a lookup rather than a search.
    pub app_id: Option<String>,
}

/// Where each storefront keeps its records. Separated from discovery so tests
/// and the diagnostics report can describe what was searched.
#[derive(Debug, Clone, Default)]
pub struct Roots {
    pub steam: Option<PathBuf>,
    pub epic_manifests: Option<PathBuf>,
    pub xbox: Vec<PathBuf>,
}

/// Merge results, preferring the first source that claimed a folder.
///
/// Two stores can name the same directory - a game moved between libraries, or
/// an Xbox title also present in Steam - and showing it twice invites somebody
/// to install into it twice.
pub fn dedupe(games: Vec<Game>) -> Vec<Game> {
    let mut out: Vec<Game> = Vec::new();
    for game in games {
        let key = normalise(&game.dir);
        if out.iter().any(|existing| normalise(&existing.dir) == key) {
            continue;
        }
        out.push(game);
    }
    // Case-insensitively, so "alpha" and "Zeta" sort the way a person reads
    // them rather than by byte value.
    out.sort_by_key(|game| game.name.to_lowercase());
    out
}

fn normalise(path: &Path) -> String {
    path.to_string_lossy()
        .to_lowercase()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_owned()
}

/// Everything the given roots know about, merged.
pub fn discover(roots: &Roots) -> Vec<Game> {
    let mut games = Vec::new();
    if let Some(steam) = &roots.steam {
        games.extend(steam::discover(steam));
    }
    if let Some(manifests) = &roots.epic_manifests {
        games.extend(epic::discover(manifests));
    }
    for root in &roots.xbox {
        games.extend(xbox::discover(root));
    }
    dedupe(games)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game(name: &str, dir: &str, source: Source) -> Game {
        Game {
            name: name.to_owned(),
            dir: PathBuf::from(dir),
            source,
            app_id: None,
        }
    }

    #[test]
    fn the_same_folder_from_two_stores_is_one_entry() {
        let merged = dedupe(vec![
            game("A Game", r"D:\Games\A Game", Source::Steam),
            game("A Game", r"d:/games/a game", Source::Xbox),
        ]);
        assert_eq!(merged.len(), 1);
        // The first source to claim it wins.
        assert_eq!(merged.first().map(|g| g.source), Some(Source::Steam));
    }

    #[test]
    fn different_folders_are_kept_and_sorted_by_name() {
        let merged = dedupe(vec![
            game("Zeta", r"D:\Games\Zeta", Source::Steam),
            game("alpha", r"D:\Games\Alpha", Source::Epic),
            game("Mid", r"D:\Games\Mid", Source::Xbox),
        ]);
        let names: Vec<&str> = merged.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "Mid", "Zeta"]);
    }

    #[test]
    fn a_trailing_separator_does_not_make_a_second_entry() {
        let merged = dedupe(vec![
            game("A", r"D:\Games\A", Source::Steam),
            game("A", r"D:\Games\A\", Source::Steam),
        ]);
        assert_eq!(merged.len(), 1);
    }

    #[test]
    fn source_labels_are_what_a_person_would_recognise() {
        assert_eq!(Source::Steam.label(), "Steam");
        assert_eq!(Source::Epic.label(), "Epic Games");
        assert_eq!(Source::Xbox.label(), "Xbox");
        assert_eq!(Source::Manual.label(), "Added by hand");
    }
}
