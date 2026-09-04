//! Traces other tools have left in a game folder.
//!
//! This matters more than it sounds, and it was found by looking at real
//! folders rather than by reasoning. A game on the development machine holds
//! `nvngx_dlssnr.dll` beside `nvngx_dlssnr.dll.original`, and both games hold
//! `nvngx.dll_dlssnr.dll`. Those are RHI's backup convention and OptiScaler's
//! naming: two other tools have already been here.
//!
//! Three consequences:
//!
//! 1. **A runtime with a `.original` sibling was installed, not shipped.** That
//!    is stronger evidence than the version-cohort heuristic, which cannot see
//!    it when the installed file happens to match its neighbours' versions.
//!
//! 2. **Our backup would capture the wrong file.** Installing over another
//!    tool's work means the copy we set aside is *that tool's* file, not the
//!    game's. Restoring later would put their swap back and call it the
//!    original. Their `.original` is the genuine article, and a user who then
//!    uninstalls that tool has no route home at all.
//!
//! 3. **Two injectors can fight over one filename.** ReShade normally owns
//!    `dxgi.dll`; OptiScaler is renamed to whichever proxy the game loads.
//!    Both claiming the same name is a crash, not a merge.
//!
//! So this reports what is already there and lets the preflight say so before
//! anything is written.

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// A tool whose traces we recognise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Tool {
    ReShade,
    OptiScaler,
    Dlss5Feeder,
    Dlss5Bridge,
    RenoDx,
    Rtx40MfgUnlock,
    UltimateAsiLoader,
    DgVoodoo,
    /// Upstream DLSS5-Swapper, which keeps its own backup directory.
    Dlss5Swapper,
    /// A `.original` or `.bak` sibling, which several tools leave behind. The
    /// convention is shared, so the tool cannot be named from it alone.
    UnknownSwapper,
}

impl Tool {
    pub const fn label(self) -> &'static str {
        match self {
            Tool::ReShade => "ReShade",
            Tool::OptiScaler => "OptiScaler",
            Tool::Dlss5Feeder => "DLSS 5 Feeder",
            Tool::Dlss5Bridge => "DLSS 5 DX11 Bridge",
            Tool::RenoDx => "RenoDX",
            Tool::Rtx40MfgUnlock => "RTX 40 MFG Unlock",
            Tool::UltimateAsiLoader => "Ultimate ASI Loader",
            Tool::DgVoodoo => "dgVoodoo 2",
            Tool::Dlss5Swapper => "DLSS5-Swapper",
            Tool::UnknownSwapper => "another swapping tool",
        }
    }

    /// Whether this tool injects itself by taking a graphics DLL's name, and
    /// so can collide with another that wants the same one.
    pub const fn claims_a_proxy_name(self) -> bool {
        matches!(
            self,
            Tool::ReShade | Tool::OptiScaler | Tool::UltimateAsiLoader | Tool::DgVoodoo
        )
    }
}

/// What was found, and where.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Footprint {
    pub tool: Tool,
    /// The file or directory that gave it away, relative to the folder.
    pub evidence: String,
}

/// Runtimes that another tool has demonstrably replaced, with the backup it
/// left behind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Displaced {
    /// The runtime now in place, which some tool put there.
    pub file: String,
    /// The copy that tool set aside. This is the genuine original.
    pub backup: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Survey {
    pub tools: Vec<Footprint>,
    /// Files another tool replaced, each with the original it kept.
    pub displaced: Vec<Displaced>,
}

impl Survey {
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty() && self.displaced.is_empty()
    }

    /// Whether anything here would make our own backup capture somebody else's
    /// file rather than the game's.
    pub fn would_shadow_a_backup(&self) -> bool {
        !self.displaced.is_empty()
    }

    pub fn tools_present(&self) -> Vec<Tool> {
        let mut found: Vec<Tool> = self.tools.iter().map(|entry| entry.tool).collect();
        found.sort_unstable();
        found.dedup();
        found
    }
}

/// Extensions other tools use for the copy they set aside.
const BACKUP_SUFFIXES: [&str; 3] = [".original", ".bak", ".dlsss"];

/// Exact filenames that identify a tool, lower-cased.
const BY_NAME: [(&str, Tool); 16] = [
    ("reshade64.dll", Tool::ReShade),
    ("reshade32.dll", Tool::ReShade),
    ("reshade.ini", Tool::ReShade),
    ("optiscaler.dll", Tool::OptiScaler),
    ("optiscaler.ini", Tool::OptiScaler),
    ("optiscaler.asi", Tool::OptiScaler),
    // OptiScaler's own naming for the runtime it carries. Seen in both games
    // on the development machine.
    ("nvngx.dll_dlssnr.dll", Tool::OptiScaler),
    ("dlss5-feed.addon64", Tool::Dlss5Feeder),
    ("dlss5-feed.addon32", Tool::Dlss5Feeder),
    ("dlss5-feed-host64.exe", Tool::Dlss5Feeder),
    ("dlss5-feed.log", Tool::Dlss5Feeder),
    ("dlss5-dx11-bridge.addon64", Tool::Dlss5Bridge),
    ("rtx40mfg.asi", Tool::Rtx40MfgUnlock),
    ("rtx40mfgcore.dll", Tool::Rtx40MfgUnlock),
    ("rtx40mfg-ui.addon64", Tool::Rtx40MfgUnlock),
    ("dgvoodoo.conf", Tool::DgVoodoo),
];

/// Directory names that identify a tool.
const BY_DIRECTORY: [(&str, Tool); 4] = [
    ("_dlss5_backup", Tool::Dlss5Swapper),
    ("reshade-shaders", Tool::ReShade),
    ("optiscaler", Tool::OptiScaler),
    ("d3d12_optiscaler", Tool::OptiScaler),
];

fn identify(lower: &str) -> Option<Tool> {
    if let Some((_, tool)) = BY_NAME.iter().find(|(name, _)| *name == lower) {
        return Some(*tool);
    }
    // RenoDX ships as `renodx-<something>.addon64`, so it is a prefix rather
    // than a fixed name.
    if lower.starts_with("renodx") && lower.contains(".addon") {
        return Some(Tool::RenoDx);
    }
    None
}

/// Inspect one directory - the folder the runtime would be installed into.
///
/// Not recursive, and deliberately so: everything here matters only when it
/// sits beside the executable that loads it, and walking a whole game to find
/// a stray `.original` would report things that cannot affect an install.
pub fn survey(directory: &Path) -> Survey {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Survey::default();
    };

    let mut names: Vec<(String, String, bool)> = Vec::new();
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let is_dir = entry.file_type().is_ok_and(|kind| kind.is_dir());
        names.push((name.to_lowercase(), name, is_dir));
    }

    let present: BTreeSet<&str> = names.iter().map(|(lower, _, _)| lower.as_str()).collect();
    let mut tools: Vec<Footprint> = Vec::new();
    let mut displaced: Vec<Displaced> = Vec::new();

    for (lower, original, is_dir) in &names {
        if *is_dir {
            if let Some((_, tool)) = BY_DIRECTORY.iter().find(|(name, _)| name == lower) {
                tools.push(Footprint {
                    tool: *tool,
                    evidence: original.clone(),
                });
            }
            continue;
        }

        if let Some(tool) = identify(lower) {
            tools.push(Footprint {
                tool,
                evidence: original.clone(),
            });
        }

        // A backup sibling. The file it shadows is what some tool replaced,
        // and it only counts when that file is actually still there - an
        // orphaned backup says the swap was already undone.
        for suffix in BACKUP_SUFFIXES {
            if let Some(stem) = lower.strip_suffix(suffix) {
                if present.contains(stem) {
                    displaced.push(Displaced {
                        file: original
                            .get(..original.len() - suffix.len())
                            .unwrap_or(stem)
                            .to_owned(),
                        backup: original.clone(),
                    });
                    tools.push(Footprint {
                        tool: Tool::UnknownSwapper,
                        evidence: original.clone(),
                    });
                }
                break;
            }
        }
    }

    // An inference across the listing rather than about a single file: any
    // `.asi` present means something is loading ASI plugins, which is the
    // loader's whole job - and it holds even when the loader itself has been
    // renamed to a proxy name we would not recognise. Kept out of `identify`,
    // which answers "what is this one file" and returns early on a name it
    // already knows.
    if let Some((_, original, _)) = names
        .iter()
        .find(|(lower, _, is_dir)| !*is_dir && lower.ends_with(".asi"))
    {
        tools.push(Footprint {
            tool: Tool::UltimateAsiLoader,
            evidence: original.clone(),
        });
    }

    tools.sort_by(|left, right| (left.tool, &left.evidence).cmp(&(right.tool, &right.evidence)));
    tools.dedup();
    displaced.sort_by(|left, right| left.file.cmp(&right.file));
    displaced.dedup();

    Survey { tools, displaced }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(files: &[&str], dirs: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        for name in files {
            std::fs::write(dir.path().join(name), b"x").expect("write");
        }
        for name in dirs {
            std::fs::create_dir_all(dir.path().join(name)).expect("dir");
        }
        dir
    }

    #[test]
    fn a_clean_game_folder_reports_nothing() {
        let dir = folder(&["Game.exe", "nvngx_dlss.dll", "sl.interposer.dll"], &[]);
        let found = survey(dir.path());
        assert!(found.is_empty());
        assert!(!found.would_shadow_a_backup());
    }

    #[test]
    fn a_backup_sibling_reveals_a_runtime_somebody_else_installed() {
        // The real case: a runtime beside the copy the installing tool kept.
        // Version-cohort provenance cannot see this when the installed file
        // matches its neighbours; the sibling is unambiguous.
        let dir = folder(
            &[
                "nvngx_dlss.dll",
                "nvngx_dlssnr.dll",
                "nvngx_dlssnr.dll.original",
            ],
            &[],
        );
        let found = survey(dir.path());
        assert_eq!(
            found.displaced,
            vec![Displaced {
                file: "nvngx_dlssnr.dll".to_owned(),
                backup: "nvngx_dlssnr.dll.original".to_owned(),
            }]
        );
        assert!(found.would_shadow_a_backup());
        assert!(found.tools_present().contains(&Tool::UnknownSwapper));
    }

    #[test]
    fn an_orphaned_backup_is_not_a_live_swap() {
        // The backup is there but the file it shadowed is gone, so the swap
        // has already been undone and our own backup would capture nothing
        // belonging to anyone else.
        let dir = folder(&["nvngx_dlssnr.dll.original"], &[]);
        let found = survey(dir.path());
        assert!(found.displaced.is_empty());
        assert!(!found.would_shadow_a_backup());
    }

    #[test]
    fn optiscaler_is_recognised_by_its_own_naming() {
        // Seen in both games on the development machine.
        let dir = folder(&["nvngx.dll_dlssnr.dll", "OptiScaler.ini"], &[]);
        assert_eq!(survey(dir.path()).tools_present(), vec![Tool::OptiScaler]);
    }

    #[test]
    fn reshade_is_recognised_by_its_dll_or_its_shader_folder() {
        let by_dll = folder(&["ReShade64.dll"], &[]);
        assert!(survey(by_dll.path())
            .tools_present()
            .contains(&Tool::ReShade));

        let by_folder = folder(&[], &["reshade-shaders"]);
        assert!(survey(by_folder.path())
            .tools_present()
            .contains(&Tool::ReShade));
    }

    #[test]
    fn the_upstream_backup_directory_is_recognised() {
        let dir = folder(&[], &["_DLSS5_Backup"]);
        assert_eq!(survey(dir.path()).tools_present(), vec![Tool::Dlss5Swapper]);
    }

    #[test]
    fn addons_and_asi_plugins_name_their_tools() {
        let dir = folder(
            &[
                "dlss5-feed.addon64",
                "dlss5-dx11-bridge.addon64",
                "renodx-dlss.addon64",
                "RTX40MFG.asi",
            ],
            &[],
        );
        let tools = survey(dir.path()).tools_present();
        for expected in [
            Tool::Dlss5Feeder,
            Tool::Dlss5Bridge,
            Tool::RenoDx,
            Tool::Rtx40MfgUnlock,
            // The .asi also implies a loader is present to load it.
            Tool::UltimateAsiLoader,
        ] {
            assert!(tools.contains(&expected), "{expected:?} in {tools:?}");
        }
    }

    #[test]
    fn tools_that_take_a_proxy_name_are_flagged_as_such() {
        // Two of these claiming the same filename is a crash, not a merge.
        assert!(Tool::ReShade.claims_a_proxy_name());
        assert!(Tool::OptiScaler.claims_a_proxy_name());
        assert!(Tool::DgVoodoo.claims_a_proxy_name());
        assert!(!Tool::Dlss5Feeder.claims_a_proxy_name());
        assert!(!Tool::UnknownSwapper.claims_a_proxy_name());
    }

    #[test]
    fn a_missing_directory_is_not_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(survey(&dir.path().join("no-such-folder")).is_empty());
    }

    #[test]
    fn detection_ignores_case_but_reports_the_real_name() {
        let dir = folder(&["OPTISCALER.INI"], &[]);
        let found = survey(dir.path());
        assert_eq!(found.tools_present(), vec![Tool::OptiScaler]);
        // The evidence shown to a user is the name as it is on disk.
        assert_eq!(found.tools[0].evidence, "OPTISCALER.INI");
    }
}
