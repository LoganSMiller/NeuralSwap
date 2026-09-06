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
    /// RTX Remix, which replaces the renderer of a DirectX 8 or 9 game
    /// outright rather than injecting into it.
    RtxRemix,
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
            Tool::RtxRemix => "RTX Remix",
            Tool::UnknownSwapper => "another swapping tool",
        }
    }

    /// Whether this tool injects itself by taking a graphics DLL's name, and
    /// so can collide with another that wants the same one.
    pub const fn claims_a_proxy_name(self) -> bool {
        matches!(
            self,
            Tool::ReShade
                | Tool::OptiScaler
                | Tool::UltimateAsiLoader
                | Tool::DgVoodoo
                | Tool::RtxRemix
        )
    }
}

impl Tool {
    /// Whether this tool gets loaded by taking the name of a system DLL.
    ///
    /// The ones that do contend for a single slot per folder, so for them
    /// "installed" and "loading" are different questions. The ones that do
    /// not - a runtime the game asks for by name, a shader folder something
    /// else reads - are simply there or not.
    pub const fn loads_by_proxy(self) -> bool {
        matches!(
            self,
            Tool::ReShade
                | Tool::OptiScaler
                | Tool::UltimateAsiLoader
                | Tool::DgVoodoo
                | Tool::RtxRemix
        )
    }

    /// The catalogue entry this detected tool corresponds to, when there is
    /// one.
    ///
    /// Two vocabularies meet here: what was found on disk, and what the
    /// catalogue can install. The mapping has to be explicit because the
    /// conflict rules are written in catalogue ids, and a detected tool that
    /// silently failed to match one would be a conflict nobody was warned
    /// about.
    ///
    /// `None` for the ones with no catalogue entry - a `.original` sibling
    /// whose author cannot be named, and upstream tools we do not install.
    pub const fn component_id(self) -> Option<&'static str> {
        match self {
            Tool::ReShade => Some("reshade"),
            Tool::OptiScaler => Some("optiscaler"),
            Tool::Dlss5Feeder => Some("dlss5-feeder"),
            Tool::Dlss5Bridge => Some("dlss5-bridge"),
            Tool::RenoDx => Some("renodx"),
            Tool::Rtx40MfgUnlock => Some("rtx40-mfg-unlock"),
            Tool::UltimateAsiLoader => Some("ultimate-asi-loader"),
            Tool::DgVoodoo => Some("dgvoodoo2"),
            // Not ours to install, so not in the catalogue.
            Tool::Dlss5Swapper | Tool::RtxRemix | Tool::UnknownSwapper => None,
        }
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
    /// The injector that currently owns a proxy DLL slot, and which file.
    ///
    /// The decisive fact about an injector, and a different question from
    /// whether one has ever been here. Ready or Not on the development
    /// machine has `reshade-shaders/` **and** `OptiScaler.ini` **and** a
    /// single `dxgi.dll` - and that `dxgi.dll` is OptiScaler. ReShade is not
    /// loading in that game; what is left of it is a shader folder.
    ///
    /// Treating the leftovers as a working install is how a tool decides an
    /// add-on's dependency is already satisfied, skips it, and produces an
    /// install where nothing loads.
    pub proxy: Option<ProxySlot>,
}

/// Who owns the loading slot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxySlot {
    /// The file taking the name, e.g. `dxgi.dll`.
    pub file: String,
    /// The tool it turned out to be, when it could be identified. `None`
    /// means something is in the slot that we do not recognise - which is
    /// still decisive, because the slot is taken.
    pub owner: Option<Tool>,
    /// Whether whatever is in the slot can host ReShade add-ons.
    ///
    /// A separate question from who owns it, and the one that decides whether
    /// an add-on route works. Measured on this machine: Ready or Not's
    /// `dxgi.dll` is OptiScaler, and it mentions "ReShade" six times because
    /// it implements part of that interface - so "is it ReShade" answers yes
    /// and is useless. The add-on marker answers no, which is the truth.
    pub addon_capable: bool,
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

    /// Whether `tool` is actually in a position to load.
    ///
    /// For an injector this is the only question that matters, and it is not
    /// the same as being present. A folder can hold every file ReShade ever
    /// wrote and still not load it, because something else took the proxy
    /// slot.
    ///
    /// A tool that does not work by proxy - a runtime, a shader pack - is
    /// answered by presence, since there is no slot to contend for.
    pub fn is_loading(&self, tool: Tool) -> bool {
        if !tool.loads_by_proxy() {
            return self.tools.iter().any(|found| found.tool == tool);
        }
        self.proxy.as_ref().is_some_and(|slot| {
            if slot.owner != Some(tool) {
                return false;
            }
            // For the injector, owning the slot is not enough. ReShade ships
            // in two builds and only one loads add-ons; the whole neural
            // stack is add-ons, so the plain build gives an install where
            // every file is right and nothing happens.
            //
            // Bitness is not checked here - this survey does not know what
            // the executable is - and is settled at placement, where it is
            // known.
            tool != Tool::ReShade || slot.addon_capable
        })
    }

    /// Present, but not in a position to do anything.
    ///
    /// Worth reporting on its own: it is the difference between "you already
    /// have this" and "you have the remains of this", and a user who is told
    /// the first will not understand why nothing happened.
    pub fn is_leftovers(&self, tool: Tool) -> bool {
        tool.loads_by_proxy()
            && self.tools.iter().any(|found| found.tool == tool)
            && !self.is_loading(tool)
    }
}

/// Extensions other tools use for the copy they set aside.
const BACKUP_SUFFIXES: [&str; 3] = [".original", ".bak", ".dlsss"];

/// Exact filenames that identify a tool, lower-cased.
const BY_NAME: [(&str, Tool); 19] = [
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
    // RTX Remix: its bridge, and the config its runtime reads.
    ("nvremixbridge.exe", Tool::RtxRemix),
    ("bridge.conf", Tool::RtxRemix),
    ("rtx.conf", Tool::RtxRemix),
];

/// Directory names that identify a tool.
/// The file names an injector can take to get itself loaded.
///
/// Windows resolves a DLL beside the executable before the system copy, so a
/// library named after one the game already imports is loaded in its place and
/// forwards the real calls on. That is how ReShade, OptiScaler and Ultimate
/// ASI Loader all get in.
///
/// It is also why they collide. There is one `dxgi.dll` slot per folder, and
/// the second tool to want it either loses or overwrites the first. Ordered
/// most-likely first, which is roughly how common the API is.
pub const PROXY_NAMES: &[&str] = &[
    "dxgi.dll",
    "d3d11.dll",
    "d3d12.dll",
    "d3d9.dll",
    "opengl32.dll",
    "winmm.dll",
    "version.dll",
    "dinput8.dll",
    "dbghelp.dll",
];

/// Byte sequences that name the owner of a proxy DLL.
///
/// Checked in order, and the counts matter rather than mere presence:
/// OptiScaler's own binary mentions ReShade a handful of times because it
/// implements part of that add-on interface, so "contains ReShade" would
/// misattribute it. Measured on a real install, `dxgi.dll` in a folder with
/// both had 65 hits for OptiScaler against 6 for ReShade.
const PROXY_OWNERS: [(&str, Tool); 4] = [
    ("OptiScaler", Tool::OptiScaler),
    ("ReShade", Tool::ReShade),
    ("Ultimate ASI Loader", Tool::UltimateAsiLoader),
    ("dgVoodoo", Tool::DgVoodoo),
];

const BY_DIRECTORY: [(&str, Tool); 5] = [
    ("_dlss5_backup", Tool::Dlss5Swapper),
    ("reshade-shaders", Tool::ReShade),
    ("optiscaler", Tool::OptiScaler),
    ("d3d12_optiscaler", Tool::OptiScaler),
    (".trex", Tool::RtxRemix),
];

/// What a single directory entry names, judged by its name alone.
///
/// The cheap half of [`survey`], and the same tables, so the two can never
/// disagree about what a name means. It exists because a caller that only wants
/// to know whether one tool is somewhere in a tree cannot afford a full survey
/// per directory - that opens and reads every proxy DLL it finds, which is the
/// right price for the folder being installed into and much too high for a walk
/// across a game's subdirectories.
///
/// Says nothing about whether the tool still works. `survey` answers that.
pub fn names_a_tool(entry_name: &str, is_dir: bool) -> Option<Tool> {
    let lower = entry_name.to_lowercase();
    if is_dir {
        return BY_DIRECTORY
            .iter()
            .find(|(name, _)| *name == lower)
            .map(|(_, tool)| *tool);
    }
    identify(&lower)
}

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

    // Who holds the loading slot. Done first so the rest of the survey can be
    // read against it: everything else here is evidence that a tool has been
    // in this folder, and this is the one fact about whether it still works.
    let proxy = PROXY_NAMES
        .iter()
        .find(|name| present.contains(**name))
        .map(|name| {
            let (owner, addon_capable) = examine_proxy(&directory.join(name));
            ProxySlot {
                file: (*name).to_owned(),
                owner,
                addon_capable,
            }
        });

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

    Survey {
        tools,
        displaced,
        proxy,
    }
}

/// What a file claiming to be ReShade actually is.
///
/// Being ReShade is not enough. ReShade ships in two builds and only one of
/// them loads add-ons; the whole neural-rendering stack is add-ons, so the
/// plain build produces an install where every file is correct and nothing
/// happens. Nor is the right build enough on its own - a 64-bit DLL does not
/// load in a 32-bit process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InjectorCheck {
    pub is_reshade: bool,
    /// Whether this build loads add-ons.
    pub has_addon_support: bool,
    /// 32 or 64, or `None` when the header could not be read.
    pub bitness: Option<u8>,
}

impl InjectorCheck {
    /// Whether this file can host add-ons for an executable of `bitness`.
    pub fn usable_for(&self, bitness: u8) -> bool {
        self.is_reshade && self.has_addon_support && self.bitness == Some(bitness)
    }
}

/// The marker that distinguishes the add-on build.
///
/// Taken from DLSS5-Swapper's `isAddonReShade`, which tests for exactly this
/// string, and confirmed against the shipping 6.8.0 add-on installer: it
/// appears once in `ReShade64.dll` and once in `ReShade32.dll`, alongside 36
/// occurrences of `ReShade` itself.
///
/// The negative case is untested here - the plain build is not distributed in
/// the add-on installer, so there is nothing on this machine to check it
/// against. The check is therefore only as good as that upstream reading, and
/// it is recorded as such rather than presented as verified both ways.
const ADDON_MARKER: &str = "Searching for add-ons";

/// Inspect a DLL that is supposed to be ReShade.
///
/// A missing or unreadable file reports everything false rather than failing:
/// this answers "can this be used", and "we could not tell" and "no" lead to
/// the same action - install a build we do know about.
pub fn inspect_injector(path: &Path) -> InjectorCheck {
    let Ok(bytes) = std::fs::read(path) else {
        return InjectorCheck {
            is_reshade: false,
            has_addon_support: false,
            bitness: None,
        };
    };
    InjectorCheck {
        is_reshade: memchr::memmem::find(&bytes, b"ReShade").is_some(),
        has_addon_support: memchr::memmem::find(&bytes, ADDON_MARKER.as_bytes()).is_some(),
        bitness: crate::pe::PeFile::with(path, |pe| Some(pe.bitness()), None),
    }
}

/// Which tool a proxy DLL turned out to be.
///
/// Counted rather than merely found. OptiScaler implements part of ReShade's
/// add-on interface and so carries the string; measured on a real install, its
/// `dxgi.dll` had 65 occurrences of `OptiScaler` against 6 of `ReShade`, and a
/// first-match rule that happened to check ReShade first would have named the
/// wrong owner.
///
/// A file we cannot read, or read and do not recognise, gives `None` - the
/// slot is still taken, which is the part that matters.
fn examine_proxy(path: &Path) -> (Option<Tool>, bool) {
    // These are DLLs, not model blobs. A cap keeps a folder with something
    // enormous sitting under a proxy name from turning a scan into a read of
    // the whole file, and 64 MiB is far past any real injector.
    const MOST: u64 = 64 * 1024 * 1024;
    if std::fs::metadata(path).is_ok_and(|meta| meta.len() > MOST) {
        return (None, false);
    }
    let Ok(bytes) = std::fs::read(path) else {
        return (None, false);
    };

    let owner = PROXY_OWNERS
        .iter()
        .map(|(needle, tool)| {
            let hits = memchr::memmem::find_iter(&bytes, needle.as_bytes()).count();
            (hits, *tool)
        })
        .filter(|(hits, _)| *hits > 0)
        .max_by_key(|(hits, _)| *hits)
        .map(|(_, tool)| tool);

    // One read answers both questions. The file can be tens of megabytes, and
    // reading it twice to ask two things about the same bytes would be a
    // scan nobody thanks us for.
    let addon_capable = memchr::memmem::find(&bytes, ADDON_MARKER.as_bytes()).is_some();
    (owner, addon_capable)
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

    /// A folder whose proxy DLL carries the given marker counts.
    fn with_proxy(name: &str, markers: &[(&str, usize)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut body = Vec::new();
        for (needle, times) in markers {
            for _ in 0..*times {
                body.extend_from_slice(needle.as_bytes());
                body.push(0);
            }
        }
        std::fs::write(dir.path().join(name), &body).expect("write");
        dir
    }

    #[test]
    fn the_proxy_slot_owner_is_decided_by_count_not_by_presence() {
        // Measured on a real install: OptiScaler's `dxgi.dll` mentions
        // ReShade six times, because it implements part of that add-on
        // interface, against sixty-five mentions of itself. A
        // "contains ReShade" rule names the wrong owner, and the consequence
        // is telling a user their ReShade is working when it is inert.
        let dir = with_proxy("dxgi.dll", &[("ReShade", 6), ("OptiScaler", 65)]);
        let survey = survey(dir.path());

        let slot = survey.proxy.as_ref().expect("a slot");
        assert_eq!(slot.file, "dxgi.dll");
        assert_eq!(slot.owner, Some(Tool::OptiScaler));
        assert!(survey.is_loading(Tool::OptiScaler));
        assert!(!survey.is_loading(Tool::ReShade));
    }

    #[test]
    fn a_reshade_without_add_on_support_is_not_a_working_injector() {
        // ReShade ships in two builds and only one loads add-ons. The whole
        // neural stack is add-ons, so the plain build gives an install where
        // every file is correct and nothing happens - the exact failure this
        // project exists to remove, produced by an install that succeeded.
        //
        // Confirmed against the shipping 6.8.0 add-on installer: the marker
        // appears once in ReShade64.dll and once in ReShade32.dll.
        let plain = with_proxy("dxgi.dll", &[("ReShade", 36)]);
        let found = survey(plain.path());
        assert_eq!(
            found.proxy.as_ref().and_then(|slot| slot.owner),
            Some(Tool::ReShade)
        );
        assert!(!found.is_loading(Tool::ReShade), "no add-on support");

        let addon = with_proxy("dxgi.dll", &[("ReShade", 36), ("Searching for add-ons", 1)]);
        assert!(survey(addon.path()).is_loading(Tool::ReShade));
    }

    #[test]
    fn mentioning_reshade_is_not_being_reshade() {
        // Measured on this machine: Ready or Not's `dxgi.dll` is OptiScaler,
        // and it carries the string "ReShade" six times because it implements
        // part of that add-on interface. So "does it mention ReShade" answers
        // yes and is useless; the add-on marker answers no, which is true.
        let dir = with_proxy("dxgi.dll", &[("ReShade", 6), ("OptiScaler", 65)]);
        let survey = survey(dir.path());

        let slot = survey.proxy.as_ref().expect("a slot");
        assert_eq!(slot.owner, Some(Tool::OptiScaler));
        assert!(
            !slot.addon_capable,
            "OptiScaler cannot host ReShade add-ons"
        );
        assert!(!survey.is_loading(Tool::ReShade));
    }

    #[test]
    fn an_injector_is_judged_on_build_and_bitness_together() {
        // A 64-bit DLL does not load in a 32-bit process, however right the
        // build is.
        let check = InjectorCheck {
            is_reshade: true,
            has_addon_support: true,
            bitness: Some(64),
        };
        assert!(check.usable_for(64));
        assert!(!check.usable_for(32));

        // And the right bitness of the wrong build is no better.
        let plain = InjectorCheck {
            is_reshade: true,
            has_addon_support: false,
            bitness: Some(64),
        };
        assert!(!plain.usable_for(64));

        // A file we could not read at all answers no, because "we cannot
        // tell" and "no" lead to the same action.
        let unreadable = inspect_injector(Path::new("no-such-file-anywhere.dll"));
        assert!(!unreadable.usable_for(64));
        assert!(!unreadable.usable_for(32));
    }

    #[test]
    fn leftovers_are_present_but_not_loading() {
        // The Ready or Not case: a `reshade-shaders/` folder and an
        // OptiScaler proxy. ReShade has been here; ReShade is not running.
        let dir = with_proxy("dxgi.dll", &[("OptiScaler", 20)]);
        std::fs::create_dir_all(dir.path().join("reshade-shaders")).expect("dir");
        let survey = survey(dir.path());

        assert!(survey.tools_present().contains(&Tool::ReShade));
        assert!(!survey.is_loading(Tool::ReShade));
        assert!(survey.is_leftovers(Tool::ReShade));
        // And the tool that does hold the slot is not "leftovers".
        assert!(!survey.is_leftovers(Tool::OptiScaler));
    }

    #[test]
    fn an_unrecognised_proxy_still_counts_as_taking_the_slot() {
        // Something is in the slot. We cannot say what, and it does not
        // matter: the name is taken, so an injector we install would have to
        // displace it.
        let dir = with_proxy("d3d11.dll", &[("something else entirely", 3)]);
        let survey = survey(dir.path());

        let slot = survey.proxy.as_ref().expect("a slot");
        assert_eq!(slot.file, "d3d11.dll");
        assert_eq!(slot.owner, None);
    }

    #[test]
    fn a_folder_with_no_proxy_dll_has_a_free_slot() {
        let dir = folder(&["Game.exe", "nvngx_dlss.dll"], &[]);
        assert!(survey(dir.path()).proxy.is_none());
    }

    #[test]
    fn a_runtime_is_judged_by_presence_because_it_contends_for_nothing() {
        // Only the proxy-loaded tools have a slot to lose. A runtime the game
        // asks for by name either exists or does not.
        assert!(!Tool::Dlss5Swapper.loads_by_proxy());
        assert!(Tool::ReShade.loads_by_proxy());
        assert!(Tool::OptiScaler.loads_by_proxy());
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
