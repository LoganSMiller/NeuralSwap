//! Deciding which files in a game folder are worth opening, and which folders
//! are worth walking into.
//!
//! A modern game folder holds tens of thousands of files, almost none of them
//! the executable. Both lists here exist to keep a library scan proportional
//! to the interesting part of the tree rather than to its size - and to stop
//! the scanner nominating an uninstaller as the game.

/// Folders that never hold the game executable.
///
/// Asset trees are the big win: a packaged Unreal title keeps tens of
/// thousands of files under `Paks` and none of them is a PE worth reading.
/// The rest are correctness rather than speed - a scan that wanders into
/// `EasyAntiCheat` or a user's own `backup` folder can nominate the wrong
/// binary, or worse, offer to patch a copy someone parked for safekeeping.
pub const SKIP_DIRS: &[&str] = &[
    // Ours.
    "_neuralswap_backup",
    "reshade-shaders",
    "host64",
    // Development leftovers.
    "node_modules",
    ".git",
    // Assets: enormous, and never the executable.
    "paks",
    "movies",
    "screenshots",
    "saved",
    "logs",
    // Managed by other tools.
    "mods",
    "downloads",
    "overwrite",
    "profiles",
    // Redistributables, which ship their own executables.
    "_redist",
    "_commonredist",
    "prerequisites",
    "directx",
    "directx_redist",
    "redist",
    "redistributable",
    "redistributables",
    "dotnet",
    "vcredist",
    "installer",
    "installers",
    "installer_resources",
    "support",
    "_support",
    // Anti-cheat helpers, which must never be nominated or walked.
    "easyanticheat",
    "eaanticheat",
    "battleye",
    // Never touch a copy the user (or another tool) parked as a backup.
    "backup",
    "backups",
    "_backup",
    "bak",
    "old",
    "original",
    "originals",
];

/// Executable name stems that are never the game itself.
///
/// Matched as a prefix on the file stem, case-insensitively, which is how
/// `UnityCrashHandler64.exe` and `unins000.exe` are both excluded without
/// listing every variant.
const NOT_A_GAME: &[&str] = &[
    "unins",
    "uninstall",
    "setup",
    "install",
    "vcredist",
    "vc_redist",
    "dxsetup",
    "dxwebsetup",
    "oalinst",
    "dotnetfx",
    "touchup",
    "crashreport",
    "crashhandler",
    "unitycrashhandler",
    // Chromium's crash handler, shipped by everything built on Electron or
    // CEF - and by a good number of games. It caused a real misidentification:
    // the NVIDIA driver has a profile for Twitch Studio that lists
    // `crashpad_handler.exe`, so asking the driver about it made an install
    // into Slay the Spire 2 report Twitch Studio's settings.
    "crashpad",
    // The .NET runtime's dump helper, shipped beside anything self-contained.
    "createdump",
    "easyanticheat",
    "eac",
    "battleye",
    "be_service",
    "launcher",
    "redlauncher",
    "activation",
    "patch",
    "update",
    "autorun",
    "autoplay",
    "readme",
    "config",
    "benchmark",
    "report",
    "helper",
    "service",
    "cleanup",
    "modorganizer",
    "steamerrorreporter",
    "dgvoodoocpl",
    "reshade_setup",
    "quicksfv",
    "rapidcrc",
];

/// Names that are the game despite matching a `NOT_A_GAME` prefix.
///
/// Without an exception list, the only way to fix a title whose executable
/// happens to begin with one of those stems is to weaken the prefix for
/// everyone.
const DESPITE_THE_PREFIX: &[&str] = &["installation", "updatestation"];

/// Stems that *end* in a helper word, which the prefix list cannot catch.
///
/// The prefix list is anchored at the start, so `Launcher.exe` is excluded but
/// `EpicGamesLauncher.exe`, `RockstarLauncher.exe` and `GameLauncher.exe` are
/// not - and those are the names real storefronts and engines actually ship.
///
/// These are **demoted, not excluded**. A wrong exclusion means a game is
/// never detected at all, which is the worst outcome the scanner has; a wrong
/// demotion only costs it first place in a list the user can see. Opening a
/// handful of extra executables is cheap, so the safe side is cheap too.
const LIKELY_HELPER_SUFFIXES: &[&str] = &[
    "launcher",
    "crashhandler",
    "crashreporter",
    "uninstaller",
    "updater",
];

/// Architecture and bitness decorations that sit after the meaningful part of
/// a name: `Launcher64`, `GameLauncher_x64`, `Foo-Win64`.
const ARCH_SUFFIXES: &[&str] = &[
    "_x64", "_x86", "-x64", "-x86", "-win64", "-win32", "_64", "_32",
];

/// Strip the decorations so a suffix rule sees the word it is looking for.
///
/// Without this, `EpicGamesLauncher64.exe` ends in "64" rather than
/// "launcher" and slips past the check entirely.
fn meaningful_stem(file_name: &str) -> String {
    let mut stem = stem_of(file_name);
    loop {
        let before = stem.len();
        for suffix in ARCH_SUFFIXES {
            if let Some(trimmed) = stem.strip_suffix(suffix) {
                stem = trimmed.to_owned();
                break;
            }
        }
        stem = stem
            .trim_end_matches(|c: char| c.is_ascii_digit())
            .to_owned();
        if stem.len() == before {
            return stem;
        }
    }
}

/// True when a name looks like a launcher or helper but is not certain enough
/// to exclude. Such candidates are ranked last rather than dropped.
pub fn is_likely_helper(file_name: &str) -> bool {
    let stem = meaningful_stem(file_name);
    LIKELY_HELPER_SUFFIXES
        .iter()
        .any(|suffix| stem.ends_with(suffix))
}

fn stem_of(file_name: &str) -> String {
    file_name
        .rsplit_once('.')
        .map_or(file_name, |(stem, _)| stem)
        .to_lowercase()
}

/// True when a file name is a helper, installer or launcher rather than a game.
pub fn is_probably_not_a_game(file_name: &str) -> bool {
    let stem = stem_of(file_name);
    if DESPITE_THE_PREFIX
        .iter()
        .any(|allowed| stem.starts_with(allowed))
    {
        return false;
    }
    NOT_A_GAME.iter().any(|prefix| stem.starts_with(prefix))
}

/// True when a directory should not be walked into.
pub fn should_skip_dir(name: &str) -> bool {
    let folded = name.to_lowercase();
    SKIP_DIRS.iter().any(|skip| *skip == folded)
}

/// `Content` is the awkward case.
///
/// Unreal's `Content` tree is assets-only and enormous, so skipping it makes
/// packaged titles far cheaper to scan. But a modern Xbox/Game Pass install
/// puts the *entire accessible game* under `<Game>\Content`, so skipping it
/// there finds nothing at all. The caller knows which layout it is looking at.
pub fn should_skip_content(name: &str, xbox_layout: bool) -> bool {
    !xbox_layout && name.eq_ignore_ascii_case("content")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installers_and_helpers_are_excluded() {
        for name in [
            "unins000.exe",
            "UnityCrashHandler64.exe",
            "vc_redist.x64.exe",
            "EasyAntiCheat_Setup.exe",
            "Launcher.exe",
            "steamerrorreporter.exe",
            "DXSETUP.exe",
        ] {
            assert!(is_probably_not_a_game(name), "{name} should be excluded");
        }
    }

    #[test]
    fn real_game_executables_are_kept() {
        for name in [
            "Cyberpunk2077.exe",
            "witcher3.exe",
            "eldenring.exe",
            "RDR2.exe",
            "BorderlandsGOTY.exe",
            // Begins with "be" but is not BattlEye's `be_service`.
            "Beyond.exe",
            // Contains "patch" but does not begin with it.
            "DispatchGame.exe",
        ] {
            assert!(!is_probably_not_a_game(name), "{name} should be kept");
        }
    }

    #[test]
    fn the_exception_list_rescues_names_that_start_with_a_prefix() {
        // "installation" starts with "install", which would otherwise exclude
        // it. The exception list is how one title is fixed without weakening
        // the prefix for every other game.
        assert!(!is_probably_not_a_game("Installation.exe"));
        // The prefix itself still excludes the ordinary case.
        assert!(is_probably_not_a_game("install.exe"));
    }

    #[test]
    fn matching_is_case_insensitive_and_extension_agnostic() {
        assert!(is_probably_not_a_game("UNINS000.EXE"));
        assert!(is_probably_not_a_game("Setup"));
        assert!(is_probably_not_a_game("setup.exe"));
    }

    #[test]
    fn asset_and_backup_directories_are_skipped() {
        for name in [
            "Paks",
            "PAKS",
            "node_modules",
            "EasyAntiCheat",
            "backup",
            "originals",
        ] {
            assert!(should_skip_dir(name), "{name} should be skipped");
        }
    }

    #[test]
    fn game_directories_are_walked() {
        for name in ["Binaries", "Win64", "bin", "x64", "Engine", "game"] {
            assert!(!should_skip_dir(name), "{name} should be walked");
        }
    }

    #[test]
    fn content_is_skipped_for_unreal_but_not_for_an_xbox_layout() {
        // Unreal: assets only, and enormous.
        assert!(should_skip_content("Content", false));
        assert!(should_skip_content("content", false));
        // Xbox/Game Pass: the whole accessible game lives under Content, so
        // skipping it would find nothing at all.
        assert!(!should_skip_content("Content", true));
        // Anything else is unaffected either way.
        assert!(!should_skip_content("Binaries", false));
        assert!(!should_skip_content("Binaries", true));
    }

    #[test]
    fn storefront_launchers_are_flagged_as_helpers_rather_than_excluded() {
        // The names real storefronts and engines ship, none of which the
        // prefix list can see because it is anchored at the start.
        for name in [
            "EpicGamesLauncher.exe",
            "RockstarLauncher.exe",
            "GameLauncher.exe",
            "BethesdaNetLauncher.exe",
            "GameUpdater.exe",
            // The decorated forms, which a plain suffix check would miss
            // because they end in a digit or an architecture tag.
            "EpicGamesLauncher64.exe",
            "GameLauncher_x64.exe",
            "SomethingLauncher-Win64.exe",
        ] {
            assert!(is_likely_helper(name), "{name} should be flagged");
        }
        // `UnityCrashHandler64` ends in "64" so the suffix rule alone would
        // miss it, but the prefix list already excludes it outright.
        assert!(is_probably_not_a_game("UnityCrashHandler64.exe"));
        // Flagged, not excluded: a wrong exclusion means the game is never
        // found, which is worse than losing first place in a visible list.
        assert!(!is_probably_not_a_game("EpicGamesLauncher.exe"));
        assert!(!is_probably_not_a_game("GameLauncher.exe"));
    }

    #[test]
    fn shared_runtime_helpers_are_excluded_outright() {
        // These are not merely unlikely to be the game - they are shipped
        // verbatim by hundreds of unrelated applications, so anything that
        // identifies software by executable name will confuse them.
        //
        // The NVIDIA driver does exactly that: it has a profile for Twitch
        // Studio listing `crashpad_handler.exe`, so asking it about the one in
        // Slay the Spire 2's folder reported Twitch Studio's DLSS settings for
        // a completely different game. Excluded, not just flagged, because a
        // confident wrong answer is worse than no answer.
        for name in [
            "crashpad_handler.exe",
            "CRASHPAD_HANDLER.EXE",
            "createdump.exe",
        ] {
            assert!(is_probably_not_a_game(name), "{name} should be excluded");
        }
    }

    #[test]
    fn real_games_are_not_flagged_as_helpers() {
        for name in [
            "Cyberpunk2077.exe",
            "eldenring.exe",
            "RDR2.exe",
            "witcher3.exe",
            "Game-Win64-Shipping.exe",
        ] {
            assert!(!is_likely_helper(name), "{name} should not be flagged");
        }
    }

    #[test]
    fn the_skip_list_has_no_duplicates() {
        let mut sorted: Vec<&str> = SKIP_DIRS.to_vec();
        sorted.sort_unstable();
        let count = sorted.len();
        sorted.dedup();
        assert_eq!(sorted.len(), count, "SKIP_DIRS contains a duplicate");
        // And every entry is already lower-cased, or the comparison misses it.
        for name in SKIP_DIRS {
            assert_eq!(*name, name.to_lowercase(), "{name} is not lower-cased");
        }
    }
}
