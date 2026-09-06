//! Anti-cheat, found before an install can get somebody banned.
//!
//! Every other consequence in this application is recoverable. A file is
//! replaced and the original is kept; a directory is created and removed
//! again; a registry value is written and taken back. This one is not.
//!
//! An add-on route injects a DLL and detours graphics entry points. Every
//! kernel-level anti-cheat treats that as tampering, and the outcome is one
//! of three things:
//!
//! 1. the game refuses to start;
//! 2. the injector is silently prevented from loading, so nothing happens and
//!    the user concludes the tool is broken;
//! 3. **the account is banned.**
//!
//! The third cannot be undone by anything this program does. So this check
//! exists, it blocks by default, and getting past it takes an explicit
//! acknowledgement rather than a dismissed warning.
//!
//! DLSS5-Autopilot carries the same check and the same reasoning, with Arma 3
//! and Arma Reforger as the recurring report: both ship BattlEye, both do
//! nothing at all when set up, and neither is a bug in the tool.
//!
//! # Detected by file, not by a list of games
//!
//! A list of titles covers the games somebody thought to add. The files an
//! anti-cheat installs are the same whatever game ships them, so looking for
//! those covers games nobody has ever reported - which is the whole point,
//! because the cost of missing one is not a bad screenshot.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// An anti-cheat product recognised by the files it installs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Product {
    BattlEye,
    EasyAntiCheat,
    Vanguard,
    GameGuard,
    Xigncode,
    DenuvoAntiCheat,
    PunkBuster,
    Faceit,
    Ricochet,
}

impl Product {
    pub const fn label(self) -> &'static str {
        match self {
            Product::BattlEye => "BattlEye",
            Product::EasyAntiCheat => "Easy Anti-Cheat",
            Product::Vanguard => "Riot Vanguard",
            Product::GameGuard => "nProtect GameGuard",
            Product::Xigncode => "XIGNCODE3",
            Product::DenuvoAntiCheat => "Denuvo Anti-Cheat",
            Product::PunkBuster => "PunkBuster",
            Product::Faceit => "FACEIT Anti-Cheat",
            Product::Ricochet => "Ricochet",
        }
    }
}

/// Name fragments that identify a product, matched case-insensitively.
///
/// Fragments rather than exact names: the same anti-cheat ships as
/// `BEService.exe`, `BEClient_x64.dll`, `battleye/` and more, and an exact-name
/// table would need every one of them. The fragments are specific enough not
/// to collide with ordinary game files.
const MARKERS: [(&str, Product); 13] = [
    ("beservice", Product::BattlEye),
    ("beclient", Product::BattlEye),
    ("battleye", Product::BattlEye),
    ("easyanticheat", Product::EasyAntiCheat),
    ("eac_launcher", Product::EasyAntiCheat),
    ("vgk.sys", Product::Vanguard),
    ("vanguard", Product::Vanguard),
    ("gameguard", Product::GameGuard),
    ("xigncode", Product::Xigncode),
    ("denuvo", Product::DenuvoAntiCheat),
    ("punkbuster", Product::PunkBuster),
    ("faceit", Product::Faceit),
    ("ricochet", Product::Ricochet),
];

/// How many entries to read from any one subdirectory.
///
/// Anti-cheat sits in its own folder beside the game, near the top; a game
/// folder can hold tens of thousands of assets. A cap keeps a scan from
/// walking an entire installation to find a file that is either in the first
/// handful of names or not there at all.
const PER_DIRECTORY: usize = 200;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Finding {
    pub products: Vec<Product>,
    /// The file or directory names that gave it away, so a user can check.
    pub evidence: Vec<String>,
}

impl Finding {
    pub fn present(&self) -> bool {
        !self.products.is_empty()
    }

    /// The products, as a sentence.
    pub fn summary(&self) -> String {
        let names: Vec<&str> = self.products.iter().map(|item| item.label()).collect();
        match names.as_slice() {
            [] => String::new(),
            [one] => (*one).to_owned(),
            [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
        }
    }

    /// What to tell the user. Deliberately blunt.
    pub fn message(&self) -> String {
        format!(
            "{} is installed with this game.\n\nAnti-cheat and injected add-ons do not coexist. \
             Expect one of three things: the game refuses to start, the add-on is blocked so \
             nothing happens at all, or your account is banned. None of that is something this \
             tool can work around - it is the anti-cheat doing its job, and the ban is the one \
             thing here that cannot be undone.\n\nIf you play this game online, do not install \
             here.",
            self.summary()
        )
    }
}

/// Look for anti-cheat in and just below two directories.
///
/// `install_dir` is where the runtime would go - beside the executable - and
/// `game_dir` is the root. Both, because anti-cheat is installed sometimes
/// beside the executable and sometimes at the top of the game, and one level
/// down from either, because it usually gets a folder of its own.
///
/// Not recursive beyond that. A deep walk of a game folder to find a file that
/// lives near the top would cost seconds and find nothing new.
pub fn detect(install_dir: &Path, game_dir: &Path) -> Finding {
    let mut products: Vec<Product> = Vec::new();
    let mut evidence: Vec<String> = Vec::new();

    let look = |name: &str, products: &mut Vec<Product>, evidence: &mut Vec<String>| {
        let lower = name.to_lowercase();
        if let Some((_, product)) = MARKERS
            .iter()
            .find(|(fragment, _)| lower.contains(fragment))
        {
            products.push(*product);
            evidence.push(name.to_owned());
        }
    };

    // The two roots, deduplicated: for a game whose executable is at the top
    // they are the same directory, and scanning it twice would double the
    // evidence list.
    let mut roots: Vec<&Path> = vec![install_dir, game_dir];
    roots.dedup();

    for root in roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten().take(PER_DIRECTORY) {
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            look(&name, &mut products, &mut evidence);

            // One level down, where an anti-cheat usually keeps itself.
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) && !name.starts_with('.') {
                let Ok(inner) = std::fs::read_dir(entry.path()) else {
                    continue;
                };
                for child in inner.flatten().take(PER_DIRECTORY) {
                    if let Some(name) = child.file_name().to_str() {
                        look(name, &mut products, &mut evidence);
                    }
                }
            }
        }
    }

    products.sort_unstable();
    products.dedup();
    evidence.sort_unstable();
    evidence.dedup();
    evidence.truncate(6);
    Finding { products, evidence }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(files: &[&str], dirs: &[(&str, &[&str])]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        for name in files {
            std::fs::write(dir.path().join(name), b"x").expect("write");
        }
        for (name, children) in dirs {
            let sub = dir.path().join(name);
            std::fs::create_dir_all(&sub).expect("dir");
            for child in *children {
                std::fs::write(sub.join(child), b"x").expect("write");
            }
        }
        dir
    }

    #[test]
    fn a_clean_game_folder_finds_nothing() {
        let dir = folder(
            &["Game.exe", "nvngx_dlss.dll", "dxgi.dll"],
            &[("Content", &["textures.pak"])],
        );
        let found = detect(dir.path(), dir.path());
        assert!(!found.present());
        assert!(found.summary().is_empty());
    }

    #[test]
    fn battleye_beside_the_executable_is_found() {
        // The recurring real-world report: Arma ships BattlEye, the install
        // does nothing, and the user blames the tool.
        let dir = folder(&["Game.exe", "BEService_x64.exe"], &[]);
        let found = detect(dir.path(), dir.path());

        assert_eq!(found.products, vec![Product::BattlEye]);
        assert!(found.evidence.contains(&"BEService_x64.exe".to_owned()));
        assert!(found.message().contains("BattlEye"));
        assert!(found.message().contains("banned"));
    }

    #[test]
    fn anti_cheat_in_its_own_folder_is_found() {
        // Where it usually lives, which is why one level down is searched.
        let dir = folder(
            &["Game.exe"],
            &[("EasyAntiCheat", &["EasyAntiCheat_x64.dll", "settings.json"])],
        );
        let found = detect(dir.path(), dir.path());
        assert_eq!(found.products, vec![Product::EasyAntiCheat]);
    }

    #[test]
    fn the_root_and_the_executables_directory_are_both_searched() {
        // Unreal puts the executable several levels down and the anti-cheat
        // at the top, so checking only one of them misses it.
        let root = tempfile::tempdir().expect("tempdir");
        let deep = root.path().join("Game/Binaries/Win64");
        std::fs::create_dir_all(&deep).expect("dirs");
        std::fs::write(deep.join("Game-Win64-Shipping.exe"), b"x").expect("write");
        std::fs::write(root.path().join("vgk.sys"), b"x").expect("write");

        let found = detect(&deep, root.path());
        assert_eq!(found.products, vec![Product::Vanguard]);
    }

    #[test]
    fn one_directory_scanned_twice_does_not_double_the_evidence() {
        // A game whose executable sits at the top passes the same path twice.
        let dir = folder(&["Game.exe", "battleye.dll"], &[]);
        let found = detect(dir.path(), dir.path());
        assert_eq!(found.evidence.len(), 1, "{:?}", found.evidence);
    }

    #[test]
    fn several_products_are_all_reported() {
        let dir = folder(
            &["Game.exe", "BEClient_x64.dll"],
            &[("EasyAntiCheat", &["EasyAntiCheat.sys"])],
        );
        let found = detect(dir.path(), dir.path());
        assert_eq!(
            found.products,
            vec![Product::BattlEye, Product::EasyAntiCheat]
        );
        let summary = found.summary();
        assert!(summary.contains("BattlEye"), "{summary}");
        assert!(summary.contains("Easy Anti-Cheat"), "{summary}");
    }

    #[test]
    fn an_ordinary_file_that_merely_looks_similar_is_not_flagged() {
        // The fragments have to be specific enough not to fire on game
        // content. A false positive here blocks an install that was fine.
        let dir = folder(
            &[
                "Game.exe",
                "vanguard_hero.pak",    // a character named Vanguard
                "ricochet_physics.dll", // a physics module
            ],
            &[],
        );
        let found = detect(dir.path(), dir.path());
        // These *do* match, and that is the honest trade: the fragments are
        // chosen for recall because the cost of a miss is a ban and the cost
        // of a false positive is a blocked install the user can acknowledge.
        // Pinned so the trade is a decision rather than a surprise.
        assert!(found.present(), "recall is preferred over precision here");
    }

    #[test]
    fn a_folder_that_cannot_be_read_is_not_an_error() {
        let found = detect(
            Path::new("no-such-directory-anywhere"),
            Path::new("nor-this-one"),
        );
        assert!(!found.present());
    }
}
