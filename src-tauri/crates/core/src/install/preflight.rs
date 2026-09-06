//! Checks run before an install is allowed to start.
//!
//! Every check runs, always, even after one has already failed. That is the
//! whole design: a user who is told "the game is running", fixes it, and is
//! then told "not enough disk space", and then "this folder needs
//! administrator rights", has been made to discover the situation three times.
//! One screen, everything known, then a single decision.
//!
//! Nothing here writes anything into the game folder except a probe file it
//! removes again, and nothing here is authoritative about content - the hashes
//! are re-checked in [`super::apply`] against the bytes actually written. A
//! preflight is a courtesy to the user, not the safety mechanism. The safety
//! mechanism is the journal.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::Code;
use crate::fsx::paths::safe_path;
use crate::install::plan::{Plan, StepAction};
use crate::platform::gpu::{self, Generation};
use crate::scan::capability::Feature;
use crate::scan::{anticheat, footprints};

/// Kept as a fixed set rather than free text so the UI can explain each one
/// properly, in the user's language, with the right advice attached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CheckName {
    GameDirectory,
    StoreProtected,
    PathSafety,
    Writable,
    FilesInUse,
    DiskSpace,
    SourceFiles,
    GraphicsCard,
    OtherTools,
    DriverOverride,
    AntiCheat,
    RemixMod,
}

impl CheckName {
    /// Every variant, in the order the checks run and the user reads them.
    ///
    /// Exists for the same reason [`crate::error::Code::ALL`] does: the UI
    /// holds a sentence per check, in a different language, and neither side
    /// can tell on its own that the other has grown a case. Adding a variant
    /// here without a label there used to show the user a raw `driverOverride`;
    /// `spec/checks.json` now pins the two together.
    pub const ALL: [CheckName; 12] = [
        CheckName::GameDirectory,
        CheckName::StoreProtected,
        CheckName::PathSafety,
        CheckName::Writable,
        CheckName::FilesInUse,
        CheckName::DiskSpace,
        CheckName::SourceFiles,
        CheckName::GraphicsCard,
        CheckName::OtherTools,
        CheckName::DriverOverride,
        CheckName::AntiCheat,
        CheckName::RemixMod,
    ];

    /// The wire name, which is what the UI keys its labels on.
    pub fn as_str(&self) -> &'static str {
        match self {
            CheckName::GameDirectory => "gameDirectory",
            CheckName::StoreProtected => "storeProtected",
            CheckName::PathSafety => "pathSafety",
            CheckName::Writable => "writable",
            CheckName::FilesInUse => "filesInUse",
            CheckName::DiskSpace => "diskSpace",
            CheckName::SourceFiles => "sourceFiles",
            CheckName::GraphicsCard => "graphicsCard",
            CheckName::OtherTools => "otherTools",
            CheckName::DriverOverride => "driverOverride",
            CheckName::AntiCheat => "antiCheat",
            CheckName::RemixMod => "remixMod",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CheckOutcome {
    Pass,
    /// Worth saying, but not a reason to stop.
    Warn,
    /// Stops the install.
    Fail,
    /// Could not be determined. Never a blocker: a check we cannot run is not
    /// evidence of a problem, and treating it as one would refuse installs on
    /// perfectly good machines.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Check {
    pub name: CheckName,
    pub outcome: CheckOutcome,
    /// Diagnostic detail. The UI leads with its own text for `name`; this is
    /// the specifics - which file, how many bytes short.
    pub detail: String,
    /// The error code the install would fail with, for a `Fail`.
    pub code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Preflight {
    pub checks: Vec<Check>,
    /// True when nothing failed. `Warn` and `Unknown` do not block.
    pub ok: bool,
}

impl Preflight {
    pub fn blockers(&self) -> Vec<&Check> {
        self.checks
            .iter()
            .filter(|check| check.outcome == CheckOutcome::Fail)
            .collect()
    }
}

/// A safety margin on top of what the plan says it needs.
///
/// Two reasons: an atomic write holds the temp file and the target at the same
/// time, and a filesystem that is completely full misbehaves in ways that are
/// nobody's idea of a good time to discover. 64 MiB is cheap insurance on a
/// drive holding games.
const SPACE_MARGIN: u64 = 64 * 1024 * 1024;

pub struct Request<'a> {
    pub game_dir: &'a Path,
    pub plan: &'a Plan,
    /// Directory holding the package's files, named as the plan's steps name
    /// them.
    pub source_dir: &'a Path,
    /// Where backups will be written. Often a different volume from the game.
    pub backup_dir: &'a Path,
    /// The hardware generation this package's runtime needs, if it needs one.
    ///
    /// `None` means the package has made no claim, and the check reports that
    /// rather than inventing a requirement.
    pub requires: Option<Generation>,
    /// The user has been shown the anti-cheat warning and chosen to proceed.
    ///
    /// Off by default, and it has to be. Every other refusal here guards a
    /// recoverable state; this one guards an account ban, which nothing this
    /// program does can undo. So the safe answer is the default, and getting
    /// past it is a decision somebody made on purpose rather than a warning
    /// they scrolled past.
    pub anti_cheat_acknowledged: bool,
}

pub fn preflight(request: &Request<'_>) -> Preflight {
    // Every check, unconditionally. The order is the order the user reads
    // them in, so it runs cheapest-and-most-fundamental first.
    let checks = vec![
        check_game_directory(request.game_dir),
        check_store_protected(request.game_dir),
        check_path_safety(request),
        check_writable(request),
        check_files_in_use(request),
        check_disk_space(request),
        check_source_files(request),
        check_graphics_card(request),
        check_other_tools(request),
        check_driver_override(request),
        check_anti_cheat(request),
        check_remix(request),
    ];

    let ok = !checks
        .iter()
        .any(|check| check.outcome == CheckOutcome::Fail);
    Preflight { checks, ok }
}

/// Whether this game has an RTX Remix mod, which every route here would break.
///
/// A Remix mod replaces the game's `d3d9.dll` with a bridge and puts a
/// 150-230 MB path-tracing runtime in a `.trex` folder. The frame is traced by
/// that runtime, DLSS upscales it, and only then does a neural pass run - as a
/// stage *inside* the runtime, not as an add-on beside the game.
///
/// So two things go wrong, and DLSS5-Autopilot enforces the same hard rule for
/// the same two reasons:
///
/// 1. **ReShade crashes a Remix game before it draws a frame.** Not degrades:
///    crashes.
/// 2. On DirectX 9 an install here would **write over the Remix runtime
///    itself**, which is the mod.
///
/// This fails rather than warns because the outcome is certain rather than
/// likely. Every other route in this application produces *something*; into a
/// Remix game they produce a game that does not start.
///
/// There is no acknowledgement flag, and that is deliberate: unlike anti-cheat
/// there is no case where proceeding is what the user wants. The route that
/// works here is one this application does not have.
fn check_remix(request: &Request<'_>) -> Check {
    // The runtime sits beside the executable, which is the game root for GTA IV
    // and `bin\` for Portal RTX. So: both roots, and one level below the game
    // root, which is the same reach `anticheat` has and for the same reason -
    // the thing being looked for keeps itself in a folder of its own.
    let Some(evidence) = find_remix(&install_dir_of(request), request.game_dir) else {
        return pass(CheckName::RemixMod, "no RTX Remix mod here");
    };

    Check {
        name: CheckName::RemixMod,
        outcome: CheckOutcome::Fail,
        detail: format!(
            "This game has an RTX Remix mod - found `{evidence}`. Every route this application \
             has installs an injector, which crashes a Remix game before it draws a frame; on \
             DirectX 9 it would also write over the Remix runtime itself.\n\nOn a Remix game \
             the neural pass belongs inside the Remix runtime rather than beside the \
             executable, which is a route this application does not have yet. Remove the Remix \
             mod first, or leave this game alone."
        ),
        code: Some(Code::RemixRuntimePresent.as_str().to_owned()),
    }
}

/// Looks for an RTX Remix runtime, returning the entry that proves it.
///
/// Name-only, so it costs one `read_dir` per directory and opens nothing. Both
/// roots plus one level under each, bounded the same way `anticheat` bounds its
/// walk.
fn find_remix(install_dir: &Path, game_dir: &Path) -> Option<String> {
    let mut roots: Vec<&Path> = vec![install_dir, game_dir];
    roots.dedup();

    for root in roots {
        // `continue`, not `?`: an unreadable first root must not cancel the
        // second one.
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten().take(SCAN_LIMIT) {
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let is_dir = entry.file_type().is_ok_and(|kind| kind.is_dir());
            if footprints::names_a_tool(&name, is_dir) == Some(footprints::Tool::RtxRemix) {
                return Some(name);
            }

            // One level down. `.trex` itself is skipped as a parent: it is
            // already the answer above, and descending into a 200 MB runtime
            // to look for itself is wasted work.
            if !is_dir || name.starts_with('.') {
                continue;
            }
            let Ok(inner) = std::fs::read_dir(entry.path()) else {
                continue;
            };
            for child in inner.flatten().take(SCAN_LIMIT) {
                let Some(child_name) = child.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                let child_is_dir = child.file_type().is_ok_and(|kind| kind.is_dir());
                if footprints::names_a_tool(&child_name, child_is_dir)
                    == Some(footprints::Tool::RtxRemix)
                {
                    return Some(format!("{name}/{child_name}"));
                }
            }
        }
    }
    None
}

/// How many entries of one directory are worth reading before concluding that
/// whatever is being looked for is not in it. Matches `anticheat`.
const SCAN_LIMIT: usize = 200;

/// The directory this plan installs into, derived from its own steps.
///
/// Falls back to the game root for a plan that writes there directly.
fn install_dir_of(request: &Request<'_>) -> PathBuf {
    request
        .plan
        .steps
        .iter()
        .filter(|step| step.action != StepAction::Skip)
        .find_map(|step| {
            let rel = step.rel.replace('\\', "/");
            rel.rsplit_once('/')
                .map(|(dir, _)| request.game_dir.join(dir))
        })
        .unwrap_or_else(|| request.game_dir.to_path_buf())
}

/// Whether anti-cheat is installed with this game.
///
/// One of two checks here that fail rather than warn on something outside our
/// control - the other is the Remix rule above - and the reason is that its
/// worst outcome is the only irreversible one in the application. A replaced file has a backup; a created directory
/// is removed; a registry value is taken back. A banned account is not
/// recoverable by any amount of care in this code.
///
/// So it blocks, and `anti_cheat_acknowledged` is the only way past. That flag
/// exists so the override is a decision rather than a dismissed dialog: the UI
/// has to show what was found and ask, then run the checks again.
///
/// It reports the finding either way. A user who has acknowledged it still
/// wants to see which product was detected, and the install log should record
/// that this was known and accepted.
fn check_anti_cheat(request: &Request<'_>) -> Check {
    let install_dir = request
        .plan
        .steps
        .iter()
        .filter(|step| step.action != StepAction::Skip)
        .find_map(|step| {
            let rel = step.rel.replace('\\', "/");
            rel.rsplit_once('/')
                .map(|(dir, _)| request.game_dir.join(dir))
        })
        .unwrap_or_else(|| request.game_dir.to_path_buf());

    let found = anticheat::detect(&install_dir, request.game_dir);
    if !found.present() {
        return pass(CheckName::AntiCheat, "no anti-cheat found with this game");
    }

    if request.anti_cheat_acknowledged {
        return Check {
            name: CheckName::AntiCheat,
            outcome: CheckOutcome::Warn,
            detail: format!(
                "{} is installed here, and you have chosen to install anyway. Found: {}.",
                found.summary(),
                found.evidence.join(", ")
            ),
            code: None,
        };
    }

    Check {
        name: CheckName::AntiCheat,
        outcome: CheckOutcome::Fail,
        detail: found.message(),
        code: Some(Code::AntiCheatPresent.as_str().to_owned()),
    }
}

/// Whether the NVIDIA driver is set to supply a runtime this install writes.
///
/// This is the check that exists because of a specific silent failure. With
/// the NVIDIA App's DLSS Override on, the driver loads its own runtime from
/// its NGX store and ignores whatever is in the game folder. Every file
/// operation succeeds, the install reports success, and the game runs
/// something else. Nothing in the filesystem shows it.
///
/// It warns rather than fails. The install is harmless and reversible, the
/// user may be about to turn the override off, and the driver's settings are
/// not ours to insist on. What matters is that the outcome is *stated* - an
/// install that will have no effect is a fine thing to allow and a terrible
/// thing to leave unsaid.
///
/// Only features this plan actually writes are considered. An override on
/// super resolution is not worth mentioning to somebody installing neural
/// rendering.
fn check_driver_override(request: &Request<'_>) -> Check {
    use crate::platform::driver_profile;

    let writing: Vec<Feature> = features_written(request.plan);
    if writing.is_empty() {
        return pass(
            CheckName::DriverOverride,
            "this install does not replace a DLSS runtime",
        );
    }

    // The driver keys its profiles by executable, and NGX loads a runtime from
    // beside the executable - so the executables that matter are exactly the
    // ones in the directory being installed into. Every one is asked rather
    // than picking a favourite: which of them is "the" game executable is a
    // guess, and it does not need making. If any of them is overridden, the
    // install is at risk.
    let executables = executables_beside(request);
    let mut profiles: Vec<driver_profile::Profile> = executables
        .iter()
        .filter_map(|exe| driver_profile::for_executable(exe))
        .collect();
    // No per-game profile found, or no executable to ask about. The global
    // profile still applies to this game, so it is the answer rather than a
    // fallback.
    if profiles.is_empty() {
        profiles.extend(driver_profile::global());
    }
    if profiles.is_empty() {
        return unknown(
            CheckName::DriverOverride,
            "could not read the NVIDIA driver's settings for this game",
        );
    }

    // Which profile the clash came from decides what the user is told to do,
    // because the two are different toggles in the NVIDIA app. Saying "turn it
    // off for this game" when the setting is global sends somebody looking in
    // the wrong screen.
    let mut clashing: Vec<Feature> = Vec::new();
    let mut from_named: Option<String> = None;
    for profile in &profiles {
        let overridden: Vec<Feature> = profile
            .overridden()
            .into_iter()
            .filter(|feature| writing.contains(feature))
            .collect();
        if !overridden.is_empty() {
            if let Some(name) = profile.name.as_deref() {
                from_named = Some(name.to_owned());
            }
            clashing.extend(overridden);
        }
    }
    clashing.sort_unstable();
    clashing.dedup();

    if clashing.is_empty() {
        let named = profiles.iter().find_map(|profile| profile.name.as_deref());
        return pass(
            CheckName::DriverOverride,
            match named {
                Some(name) => {
                    format!("the driver profile \"{name}\" is not set to override these runtimes")
                }
                None => "the driver is not set to override these runtimes".to_owned(),
            },
        );
    }

    let names: Vec<&str> = clashing.iter().map(|feature| feature.label()).collect();
    let where_to_look = match from_named.as_deref() {
        Some(name) => format!("in the NVIDIA app's settings for \"{name}\""),
        None => "in the NVIDIA app's global graphics settings - it is set there rather than \
                 for this game specifically"
            .to_owned(),
    };
    Check {
        name: CheckName::DriverOverride,
        outcome: CheckOutcome::Warn,
        detail: format!(
            "DLSS Override is on for {}, so your driver will load its own runtime and ignore \
             the one installed here. Turn it off {where_to_look}, or this install will make no \
             difference.",
            list_features(&names)
        ),
        code: None,
    }
}

/// The executable file names in the directory this plan installs into.
///
/// Derived from the plan's own steps, so it names the directory the runtime
/// will actually be loaded from rather than somewhere else in the game. A
/// folder with no executable in it is normal - plenty of games keep their
/// runtimes one level down - and yields nothing rather than an error.
fn executables_beside(request: &Request<'_>) -> Vec<String> {
    let Some(dir) = request
        .plan
        .steps
        .iter()
        .filter(|step| step.action != StepAction::Skip)
        .find_map(|step| {
            let rel = step.rel.replace('\\', "/");
            rel.rsplit_once('/').map(|(dir, _)| dir.to_owned())
        })
    else {
        // Installing into the game root, where the steps carry no directory.
        return list_executables(request.game_dir);
    };
    list_executables(&request.game_dir.join(dir))
}

/// The game executables in one directory, helpers excluded.
///
/// The exclusion is not tidiness. The driver keys profiles by executable name,
/// and generic helpers are shipped by hundreds of unrelated applications - so
/// asking the driver about `crashpad_handler.exe` returns whichever profile
/// happens to claim it. On this machine that made an install into Slay the
/// Spire 2 report the DLSS Override setting for **Twitch Studio**, which is a
/// worse failure than saying nothing: it is a confident, specific, wrong
/// answer pointing the user at a screen that has nothing to do with the game.
fn list_executables(dir: &Path) -> Vec<String> {
    use crate::scan::candidates::is_probably_not_a_game;

    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.to_ascii_lowercase().ends_with(".exe"))
        .filter(|name| !is_probably_not_a_game(name))
        .collect();
    found.sort_unstable();
    found
}

/// The features whose runtime this plan writes.
///
/// Derived from the steps rather than declared, so it cannot drift from what
/// the install actually does. Skipped steps do not count: a step that writes
/// nothing cannot be overridden.
fn features_written(plan: &Plan) -> Vec<Feature> {
    let mut found: Vec<Feature> = plan
        .steps
        .iter()
        .filter(|step| step.action != StepAction::Skip)
        .filter_map(|step| {
            let name = step.rel.rsplit(['/', '\\']).next().unwrap_or("");
            Feature::from_runtime(name)
        })
        .collect();
    found.sort_unstable();
    found.dedup();
    found
}

fn list_features(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

fn pass(name: CheckName, detail: impl Into<String>) -> Check {
    Check {
        name,
        outcome: CheckOutcome::Pass,
        detail: detail.into(),
        code: None,
    }
}

fn fail_check(name: CheckName, code: Code, detail: impl Into<String>) -> Check {
    Check {
        name,
        outcome: CheckOutcome::Fail,
        detail: detail.into(),
        code: Some(code.as_str().to_owned()),
    }
}

fn unknown(name: CheckName, detail: impl Into<String>) -> Check {
    Check {
        name,
        outcome: CheckOutcome::Unknown,
        detail: detail.into(),
        code: None,
    }
}

fn check_game_directory(game_dir: &Path) -> Check {
    if game_dir.is_dir() {
        pass(CheckName::GameDirectory, game_dir.display().to_string())
    } else {
        fail_check(
            CheckName::GameDirectory,
            Code::BadRequest,
            format!("{} is not a directory", game_dir.display()),
        )
    }
}

/// Xbox and Microsoft Store titles installed under `WindowsApps` are owned by
/// `TrustedInstaller`, not the user.
///
/// Writing there means taking ownership of a system directory, which breaks
/// the store's own repair and update paths and is not something this
/// application will do to somebody's machine. Refused with an explanation
/// rather than attempted with elevation.
///
/// `C:\XboxGames`, the newer Xbox layout, is an ordinary writable folder and
/// is deliberately not caught here.
///
/// The segments are split here rather than by `Path::components`, for the same
/// reason [`crate::fsx::paths`] applies its rules without consulting the host:
/// `components` only recognises the separators of the platform it is compiled
/// for, so off Windows this whole path arrives as one segment and the check
/// silently passes. That is precisely the shape of the bug that made the path
/// validator platform-dependent - refused on Windows, accepted on Linux - and
/// a security check that answers differently by platform is not one worth
/// having, even when only one platform ships.
fn check_store_protected(game_dir: &Path) -> Check {
    let rendered = game_dir.to_string_lossy();
    let protected = rendered
        .split(['/', '\\'])
        .any(|segment| segment.eq_ignore_ascii_case("WindowsApps"));
    if protected {
        fail_check(
            CheckName::StoreProtected,
            Code::TargetProtected,
            "this title is installed under WindowsApps, which is owned by the \
             system rather than by you - installing there would mean taking \
             ownership of a protected folder",
        )
    } else {
        pass(CheckName::StoreProtected, "an ordinary writable location")
    }
}

/// Every target must still resolve inside the game folder once the filesystem
/// has its say. The plan checked the strings; this checks the disk, where a
/// junction can point anywhere.
fn check_path_safety(request: &Request<'_>) -> Check {
    for step in changing(request.plan) {
        if let Err(error) = safe_path(request.game_dir, &step.rel) {
            return Check {
                name: CheckName::PathSafety,
                outcome: CheckOutcome::Fail,
                detail: format!("{}: {}", step.rel, error.detail),
                code: Some(error.code.as_str().to_owned()),
            };
        }
    }
    pass(
        CheckName::PathSafety,
        "every target resolves inside the game folder",
    )
}

/// Prove we can write, by writing. Permissions on Windows are not reliably
/// predictable from metadata - an inherited deny ACE, a read-only attribute on
/// the folder, a drive mounted read-only - so the honest test is the attempt.
fn check_writable(request: &Request<'_>) -> Check {
    let Some(dir) = deepest_existing(request.game_dir, &request.plan.install_dir) else {
        return unknown(
            CheckName::Writable,
            "no existing directory to test yet; it will be created",
        );
    };
    let probe = dir.join(format!(".neuralswap-write-test-{}", std::process::id()));
    match fs::write(&probe, b"probe") {
        Ok(()) => {
            let _ = fs::remove_file(&probe);
            pass(CheckName::Writable, dir.display().to_string())
        }
        Err(error) => fail_check(
            CheckName::Writable,
            Code::StateUnwritable,
            format!("cannot write into {}: {error}", dir.display()),
        ),
    }
}

/// A DLL held open by a running game cannot be replaced.
///
/// Windows locks a mapped image against writing, so this is both the check and
/// the reason the install would fail. Opening for write and closing again is
/// harmless - it truncates nothing, because no truncating flag is set.
fn check_files_in_use(request: &Request<'_>) -> Check {
    let mut locked: Vec<String> = Vec::new();
    for step in changing(request.plan) {
        let target = request.game_dir.join(step.rel.replace('\\', "/"));
        if !target.is_file() {
            continue;
        }
        if let Err(error) = fs::OpenOptions::new().write(true).open(&target) {
            // ERROR_SHARING_VIOLATION and ERROR_LOCK_VIOLATION mean something
            // else has it. Anything else is a permission problem, which the
            // writable check reports more usefully.
            if matches!(error.raw_os_error(), Some(32) | Some(33)) {
                locked.push(step.rel.clone());
            }
        }
    }
    if locked.is_empty() {
        pass(CheckName::FilesInUse, "nothing is holding the target files")
    } else {
        fail_check(
            CheckName::FilesInUse,
            Code::TargetLocked,
            format!(
                "in use by another process - close the game and try again: {}",
                locked.join(", ")
            ),
        )
    }
}

/// The plan states how many bytes it will write and how many it will copy
/// aside. Those can land on different volumes, so both are checked and the
/// message names whichever is short.
fn check_disk_space(request: &Request<'_>) -> Check {
    let needed_game = request.plan.write_bytes.saturating_add(SPACE_MARGIN);
    let needed_backup = request.plan.backup_bytes;

    let game_free = crate::platform::free_space(request.game_dir);
    let backup_free = crate::platform::free_space(request.backup_dir);

    let mut shortfalls: Vec<String> = Vec::new();
    if let Some(free) = game_free {
        if free < needed_game {
            shortfalls.push(format!(
                "{} needs {} but has {}",
                request.game_dir.display(),
                bytes(needed_game),
                bytes(free)
            ));
        }
    }
    if let Some(free) = backup_free {
        if needed_backup > 0 && free < needed_backup.saturating_add(SPACE_MARGIN) {
            shortfalls.push(format!(
                "backups need {} but {} has {}",
                bytes(needed_backup),
                request.backup_dir.display(),
                bytes(free)
            ));
        }
    }

    if !shortfalls.is_empty() {
        return fail_check(
            CheckName::DiskSpace,
            Code::InsufficientSpace,
            shortfalls.join("; "),
        );
    }
    if game_free.is_none() && backup_free.is_none() {
        return unknown(CheckName::DiskSpace, "could not measure free space");
    }
    pass(
        CheckName::DiskSpace,
        format!(
            "{} to write, {} to copy aside",
            bytes(request.plan.write_bytes),
            bytes(request.plan.backup_bytes)
        ),
    )
}

/// The package must actually contain what the plan promised.
///
/// Existence and size only. Hashing every file here would read the whole
/// package twice, and `apply` verifies the hash of what it writes anyway -
/// which is the check that matters, because it sees the bytes that landed
/// rather than the bytes we intended to send.
fn check_source_files(request: &Request<'_>) -> Check {
    let mut problems: Vec<String> = Vec::new();
    for step in changing(request.plan) {
        let Some(name) = step.rel.rsplit(['/', '\\']).next() else {
            problems.push(format!("{}: no file name", step.rel));
            continue;
        };
        let source = request.source_dir.join(name);
        match fs::metadata(&source) {
            Ok(meta) if meta.is_file() && meta.len() == step.write_bytes => {}
            Ok(meta) if meta.is_file() => problems.push(format!(
                "{name} is {} but the plan expects {}",
                bytes(meta.len()),
                bytes(step.write_bytes)
            )),
            Ok(_) => problems.push(format!("{name} is not a file")),
            Err(error) => problems.push(format!("{name}: {error}")),
        }
    }
    if problems.is_empty() {
        pass(
            CheckName::SourceFiles,
            format!("{} files present", changing(request.plan).count()),
        )
    } else {
        fail_check(
            CheckName::SourceFiles,
            Code::PackageInvalid,
            problems.join("; "),
        )
    }
}

/// Whether this machine's hardware can run what is about to be installed.
///
/// A DLSS feature gated to a GPU generation is gated because it needs hardware
/// the earlier cards do not have. Installing it anyway produces a game that
/// crashes on launch or silently falls back, and the user blames whatever
/// touched the folder last - which would be us.
///
/// So this check exists to **stop** an install onto hardware that cannot run
/// it. It is deliberately not a mechanism for selecting a different build per
/// card: routing older hardware onto a runtime that has had its own hardware
/// check removed is exactly the failure this is here to prevent, and it would
/// arrive wearing our journal, manifest and restore path as if we had
/// sanctioned it.
///
/// Uncertainty never blocks. An unreadable adapter, an unrecognised name, or a
/// package that states no requirement all report rather than refuse: being
/// unable to identify a card is not evidence that it is the wrong one.
fn check_graphics_card(request: &Request<'_>) -> Check {
    let Some(required) = request.requires else {
        return unknown(
            CheckName::GraphicsCard,
            "this package does not say what hardware it needs",
        );
    };

    let Some(adapter) = gpu::best_nvidia() else {
        // No NVIDIA card at all. Worth saying loudly, but the machine may have
        // one the registry could not describe, and a laptop's discrete GPU can
        // be switched off entirely.
        return Check {
            name: CheckName::GraphicsCard,
            outcome: CheckOutcome::Warn,
            detail: format!(
                "no NVIDIA graphics card was found, and this package needs {}",
                required.label()
            ),
            code: None,
        };
    };

    if adapter.generation.at_least(required) {
        return pass(
            CheckName::GraphicsCard,
            match adapter.nvidia_driver.as_deref() {
                Some(driver) => format!("{} (driver {driver})", adapter.name),
                None => adapter.name.clone(),
            },
        );
    }

    fail_check(
        CheckName::GraphicsCard,
        Code::HardwareUnsupported,
        format!(
            "{} is {}, and this runtime needs {}",
            adapter.name,
            adapter.generation.label(),
            required.label()
        ),
    )
}

/// Whether another tool has already modified this folder.
///
/// A warning rather than a refusal - it is the user's game and they may well
/// have installed something on purpose. But the consequence is not obvious and
/// it damages the one guarantee we make.
///
/// If another tool replaced a runtime and kept its own copy of the original,
/// then the file sitting there now is *theirs*. Our backup would capture that,
/// and a restore months later would put their swap back and describe it as the
/// game's own file. Their `.original` is the genuine article - so it is named
/// here, while it still exists to be named.
fn check_other_tools(request: &Request<'_>) -> Check {
    let folder = if request.plan.install_dir.is_empty() {
        request.game_dir.to_path_buf()
    } else {
        request
            .game_dir
            .join(request.plan.install_dir.replace('\\', "/"))
    };
    let survey = footprints::survey(&folder);
    if survey.is_empty() {
        return pass(
            CheckName::OtherTools,
            "nothing else has modified this folder",
        );
    }

    let names: Vec<&str> = survey
        .tools_present()
        .iter()
        .map(|tool| tool.label())
        .collect();

    if survey.would_shadow_a_backup() {
        let shadowed: Vec<String> = survey
            .displaced
            .iter()
            .map(|entry| {
                format!(
                    "{} (their copy of the original: {})",
                    entry.file, entry.backup
                )
            })
            .collect();
        return Check {
            name: CheckName::OtherTools,
            outcome: CheckOutcome::Warn,
            detail: format!(
                "{} already replaced files here and kept the originals. If NeuralSwap \
                 installs over them, the copy it sets aside will be theirs rather than the \
                 game's - so keep their backup: {}",
                names.join(", "),
                shadowed.join("; ")
            ),
            code: None,
        };
    }

    Check {
        name: CheckName::OtherTools,
        outcome: CheckOutcome::Warn,
        detail: format!(
            "already present here: {}. Two injectors that take the same filename will not \
             both load.",
            names.join(", ")
        ),
        code: None,
    }
}

/// The steps that change something. A skip has nothing to check.
fn changing(plan: &Plan) -> impl Iterator<Item = &crate::install::plan::Step> {
    plan.steps
        .iter()
        .filter(|step| step.action != StepAction::Skip)
}

/// Walk down from the game directory as far as the install directory exists.
/// A folder that does not exist yet cannot be probed, but its parent can.
fn deepest_existing(game_dir: &Path, install_dir: &str) -> Option<PathBuf> {
    if !game_dir.is_dir() {
        return None;
    }
    let mut deepest = game_dir.to_path_buf();
    for segment in install_dir.split(['/', '\\']) {
        if segment.is_empty() || segment == "." {
            continue;
        }
        let next = deepest.join(segment);
        if !next.is_dir() {
            break;
        }
        deepest = next;
    }
    Some(deepest)
}

fn bytes(count: u64) -> String {
    const UNITS: [&str; 4] = ["B", "kB", "MB", "GB"];
    let mut value = count as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    let label = UNITS.get(unit).copied().unwrap_or("B");
    if unit == 0 {
        format!("{count} {label}")
    } else {
        format!("{value:.1} {label}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install::plan::{build_plan, PackageFile, PlanInput, Route};
    use crate::scan::folder::RuntimeKind;

    struct Fixture {
        _root: tempfile::TempDir,
        game: PathBuf,
        source: PathBuf,
        backups: PathBuf,
        plan: Plan,
    }

    /// A game with `bin/x64/` and a package offering one DLL for it.
    fn fixture(contents: &[u8]) -> Fixture {
        let root = tempfile::tempdir().expect("tempdir");
        let game = root.path().join("game");
        let source = root.path().join("package");
        let backups = root.path().join("backups");
        fs::create_dir_all(game.join("bin/x64")).expect("game dirs");
        fs::create_dir_all(&source).expect("source dir");
        fs::create_dir_all(&backups).expect("backup dir");
        fs::write(source.join("nvngx_dlss.dll"), contents).expect("source file");

        let plan = build_plan(&PlanInput {
            route: Route::NativeDll,
            install_dir: "bin/x64".to_owned(),
            present: vec![],
            pkg: vec![PackageFile {
                name: "nvngx_dlss.dll".to_owned(),
                kind: RuntimeKind::Dlss,
                version: Some("310.8.0.0".to_owned()),
                size: contents.len() as u64,
                sha256: crate::hash::hash_bytes(contents),
            }],
        })
        .expect("plan");

        Fixture {
            _root: root,
            game,
            source,
            backups,
            plan,
        }
    }

    fn run(fixture: &Fixture) -> Preflight {
        preflight(&Request {
            game_dir: &fixture.game,
            plan: &fixture.plan,
            source_dir: &fixture.source,
            backup_dir: &fixture.backups,
            requires: None,
            anti_cheat_acknowledged: false,
        })
    }

    fn outcome_of(report: &Preflight, name: CheckName) -> CheckOutcome {
        report
            .checks
            .iter()
            .find(|check| check.name == name)
            .map(|check| check.outcome)
            .expect("check present")
    }

    #[test]
    fn a_remix_mod_beside_the_executable_blocks_every_route() {
        let fixture = fixture(b"x");
        fs::create_dir_all(fixture.game.join("bin/x64/.trex")).expect("trex");

        let report = run(&fixture);

        assert_eq!(outcome_of(&report, CheckName::RemixMod), CheckOutcome::Fail);
        assert!(!report.ok, "a Remix game must not be installable");
        let check = report
            .checks
            .iter()
            .find(|check| check.name == CheckName::RemixMod)
            .expect("remix check");
        assert_eq!(check.code.as_deref(), Some("remixRuntimePresent"));
        assert!(
            check.detail.contains(".trex"),
            "the user is told what was found: {}",
            check.detail
        );
    }

    #[test]
    fn a_remix_runtime_one_level_down_is_still_found() {
        // Portal RTX keeps its runtime under `bin\`, which is neither the game
        // root nor the folder this plan installs into. An earlier version of
        // this check looked only at those two and would have passed it.
        let fixture = fixture(b"x");
        fs::create_dir_all(fixture.game.join("bin/.trex")).expect("trex");

        assert_eq!(
            outcome_of(&run(&fixture), CheckName::RemixMod),
            CheckOutcome::Fail
        );
    }

    #[test]
    fn the_remix_check_is_not_fooled_by_another_tool() {
        // The directory table holds five names and only one of them is Remix.
        // Matching on "is a tool present" rather than "is Remix present" would
        // refuse an install over a ReShade folder.
        let fixture = fixture(b"x");
        fs::create_dir_all(fixture.game.join("reshade-shaders")).expect("shaders");

        assert_eq!(
            outcome_of(&run(&fixture), CheckName::RemixMod),
            CheckOutcome::Pass
        );
    }

    #[test]
    fn a_remix_bridge_file_counts_as_much_as_the_runtime_folder() {
        // A mod mid-install, or one whose runtime sits elsewhere, still has the
        // bridge beside the executable - and the bridge is the part that
        // replaces d3d9.dll.
        let fixture = fixture(b"x");
        fs::write(fixture.game.join("bin/x64/nvremixbridge.exe"), b"x").expect("bridge");

        assert_eq!(
            outcome_of(&run(&fixture), CheckName::RemixMod),
            CheckOutcome::Fail
        );
    }

    #[test]
    fn the_features_written_come_from_the_steps() {
        // Derived rather than declared, so it cannot drift from what the
        // install actually does.
        let fixture = fixture(b"runtime bytes");
        assert_eq!(
            features_written(&fixture.plan),
            vec![Feature::SuperResolution]
        );
    }

    #[test]
    fn a_skipped_step_writes_no_feature() {
        // An identical file is skipped, and a step that writes nothing cannot
        // be overridden by anything - warning about it would be noise on the
        // one install guaranteed to change nothing anyway.
        let contents = b"runtime bytes";
        let fixture = fixture(contents);
        let mut plan = fixture.plan.clone();
        for step in &mut plan.steps {
            step.action = StepAction::Skip;
        }
        assert!(features_written(&plan).is_empty());
    }

    #[test]
    fn helper_executables_are_never_asked_about() {
        // The driver identifies software by executable name, so a helper that
        // ships with everything matches whichever profile claims it first.
        let dir = tempfile::tempdir().expect("tempdir");
        for name in [
            "SlayTheSpire2.exe",
            "crashpad_handler.exe",
            "createdump.exe",
            "UnityCrashHandler64.exe",
            "readme.txt",
        ] {
            fs::write(dir.path().join(name), b"x").expect("write");
        }

        assert_eq!(list_executables(dir.path()), vec!["SlayTheSpire2.exe"]);
    }

    #[test]
    fn the_driver_override_check_never_blocks_an_install() {
        // The invariant that matters. Whatever this machine's driver is set
        // to - and the check reads the real one - it must not be able to stop
        // an install. The driver's settings are the user's, they are
        // reversible in the NVIDIA app, and refusing to write a file over
        // them would be us insisting on something that is not ours to insist
        // on. Saying so is the whole job.
        let fixture = fixture(b"runtime bytes");
        let report = run(&fixture);
        let outcome = outcome_of(&report, CheckName::DriverOverride);
        assert_ne!(outcome, CheckOutcome::Fail);

        let check = report
            .checks
            .iter()
            .find(|check| check.name == CheckName::DriverOverride)
            .expect("the check ran");
        assert!(check.code.is_none(), "a warning carries no error code");
        assert!(!check.detail.is_empty());

        // And when it does warn, it has to say what to do about it rather
        // than only that something is wrong.
        if outcome == CheckOutcome::Warn {
            assert!(check.detail.contains("NVIDIA app"), "{}", check.detail);
        }
    }

    #[test]
    fn a_healthy_install_passes_everything() {
        let fixture = fixture(b"runtime bytes");
        let report = run(&fixture);
        assert!(report.ok, "{:?}", report.blockers());
        // Every check is reported, not just the failures - the user sees the
        // whole picture on one screen.
        assert_eq!(report.checks.len(), 12);
    }

    #[test]
    fn every_check_runs_even_after_one_fails() {
        // The point of the design: a user should learn about all of it at
        // once, rather than fixing one thing to discover the next.
        let fixture = fixture(b"runtime bytes");
        fs::remove_file(fixture.source.join("nvngx_dlss.dll")).expect("remove source");
        let broken = Fixture {
            game: fixture.game.join("no-such-folder"),
            ..fixture
        };
        let report = run(&broken);
        assert!(!report.ok);
        assert_eq!(report.checks.len(), 12);
        assert_eq!(
            outcome_of(&report, CheckName::GameDirectory),
            CheckOutcome::Fail
        );
        assert_eq!(
            outcome_of(&report, CheckName::SourceFiles),
            CheckOutcome::Fail
        );
        assert!(report.blockers().len() >= 2);
    }

    #[test]
    fn a_windowsapps_location_is_refused_with_an_explanation() {
        let fixture = fixture(b"runtime bytes");
        let protected = Fixture {
            game: PathBuf::from("C:\\Program Files\\WindowsApps\\SomePublisher.Game_1.0"),
            ..fixture
        };
        let report = run(&protected);
        let check = report
            .checks
            .iter()
            .find(|check| check.name == CheckName::StoreProtected)
            .expect("check");
        assert_eq!(check.outcome, CheckOutcome::Fail);
        assert_eq!(check.code.as_deref(), Some("targetProtected"));
        // Refused, not attempted with elevation.
        assert!(check.detail.contains("owned by the system"));
    }

    #[test]
    fn the_protected_location_check_does_not_depend_on_the_host_platform() {
        // Both separators, both cases, and a lookalike that must not match.
        // This runs on Linux in CI, where `Path::components` would see each of
        // these as a single segment and let the first two through.
        let protected =
            |path: &str| check_store_protected(Path::new(path)).outcome == CheckOutcome::Fail;
        assert!(protected(
            "C:\\Program Files\\WindowsApps\\Publisher.Game_1.0"
        ));
        assert!(protected("C:/Program Files/windowsapps/Publisher.Game_1.0"));
        assert!(!protected("C:\\XboxGames\\Some Game\\Content"));
        assert!(!protected("D:\\Games\\WindowsAppsLauncher\\game"));
    }

    #[test]
    fn the_newer_xbox_layout_is_not_treated_as_protected() {
        let fixture = fixture(b"runtime bytes");
        let xbox = Fixture {
            game: PathBuf::from("C:\\XboxGames\\Some Game\\Content"),
            ..fixture
        };
        let report = run(&xbox);
        assert_eq!(
            outcome_of(&report, CheckName::StoreProtected),
            CheckOutcome::Pass
        );
    }

    #[test]
    fn a_source_file_of_the_wrong_size_is_caught() {
        let fixture = fixture(b"runtime bytes");
        fs::write(fixture.source.join("nvngx_dlss.dll"), b"truncated").expect("shorten");
        let report = run(&fixture);
        let check = report
            .checks
            .iter()
            .find(|check| check.name == CheckName::SourceFiles)
            .expect("check");
        assert_eq!(check.outcome, CheckOutcome::Fail);
        assert_eq!(check.code.as_deref(), Some("packageInvalid"));
    }

    #[test]
    fn a_missing_install_directory_is_unknown_rather_than_a_failure() {
        // It will be created. Refusing here would block a legitimate install
        // into a folder the game happens not to have yet.
        let fixture = fixture(b"runtime bytes");
        fs::remove_dir_all(fixture.game.join("bin/x64")).expect("remove");
        let report = run(&fixture);
        assert!(report.ok, "{:?}", report.blockers());
        assert_eq!(outcome_of(&report, CheckName::Writable), CheckOutcome::Pass);
    }

    #[test]
    fn an_unmeasurable_disk_does_not_block_an_install() {
        let fixture = fixture(b"runtime bytes");
        let odd = Fixture {
            backups: PathBuf::from("\\\\?\\GLOBALROOT\\nope"),
            ..fixture
        };
        let report = run(&odd);
        // The game volume still answers, so this passes rather than blocking.
        assert_ne!(
            outcome_of(&report, CheckName::DiskSpace),
            CheckOutcome::Fail
        );
    }

    /// Run the checks with a stated hardware requirement.
    fn run_needing(fixture: &Fixture, requires: Generation) -> Preflight {
        preflight(&Request {
            game_dir: &fixture.game,
            plan: &fixture.plan,
            source_dir: &fixture.source,
            backup_dir: &fixture.backups,
            requires: Some(requires),
            anti_cheat_acknowledged: false,
        })
    }

    #[test]
    fn a_package_that_states_no_hardware_requirement_is_not_blocked() {
        let fixture = fixture(b"runtime bytes");
        let report = run(&fixture);
        assert_eq!(
            outcome_of(&report, CheckName::GraphicsCard),
            CheckOutcome::Unknown
        );
        assert!(report.ok);
    }

    #[test]
    fn hardware_older_than_the_runtime_needs_blocks_the_install() {
        // The whole reason this check exists. A feature gated to a generation
        // is gated because it needs silicon the earlier cards do not have, and
        // installing anyway produces a game that crashes on launch.
        let fixture = fixture(b"runtime bytes");
        let Some(card) = gpu::best_nvidia() else {
            return; // No NVIDIA card here; the no-card path is covered below.
        };

        // Require something strictly newer than whatever this machine has.
        let beyond = match card.generation {
            Generation::Blackwell | Generation::NewerThanKnown => None,
            _ => Some(Generation::NewerThanKnown),
        };
        let Some(beyond) = beyond else {
            // This machine is current, so assert the passing direction and the
            // refusing direction with a card we know is older.
            let report = run_needing(&fixture, Generation::Turing);
            assert_eq!(
                outcome_of(&report, CheckName::GraphicsCard),
                CheckOutcome::Pass
            );
            assert!(!Generation::Turing.at_least(Generation::Blackwell));
            return;
        };

        let report = run_needing(&fixture, beyond);
        let check = report
            .checks
            .iter()
            .find(|check| check.name == CheckName::GraphicsCard)
            .expect("a graphics check");
        assert_eq!(check.outcome, CheckOutcome::Fail);
        assert_eq!(check.code.as_deref(), Some("hardwareUnsupported"));
        assert!(!report.ok);
    }

    #[test]
    fn hardware_new_enough_passes_and_names_the_card() {
        let fixture = fixture(b"runtime bytes");
        let report = run_needing(&fixture, Generation::PreTuring);
        let check = report
            .checks
            .iter()
            .find(|check| check.name == CheckName::GraphicsCard)
            .expect("a graphics check");
        match gpu::best_nvidia() {
            // Every NVIDIA generation is at least PreTuring, so this passes
            // and the detail should say which card it decided against.
            Some(card) => {
                assert_eq!(check.outcome, CheckOutcome::Pass);
                assert!(check.detail.contains(&card.name));
            }
            // No NVIDIA card is a warning, never a block: a laptop's discrete
            // GPU can be switched off, and the registry may not describe it.
            None => assert_eq!(check.outcome, CheckOutcome::Warn),
        }
    }

    #[test]
    fn byte_counts_read_as_sizes_rather_than_digits() {
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(1_500), "1.5 kB");
        assert_eq!(bytes(20_400_000), "20.4 MB");
    }

    #[cfg(windows)]
    #[test]
    fn a_file_held_open_by_another_process_is_reported() {
        use std::os::windows::fs::OpenOptionsExt;

        let fixture = fixture(b"runtime bytes");
        let target = fixture.game.join("bin/x64/nvngx_dlss.dll");
        fs::write(&target, b"the game's own copy").expect("write target");

        // Opened with no sharing, which is how Windows holds a mapped image.
        // Held for the duration of the check, then dropped.
        const FILE_SHARE_NONE: u32 = 0;
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_NONE)
            .open(&target)
            .expect("hold the file open");

        let report = run(&fixture);
        let check = report
            .checks
            .iter()
            .find(|check| check.name == CheckName::FilesInUse)
            .expect("check");
        assert_eq!(check.outcome, CheckOutcome::Fail);
        assert_eq!(check.code.as_deref(), Some("targetLocked"));
        assert!(check.detail.contains("close the game"));

        drop(held);
        // And once it is released, the check passes.
        let after = run(&fixture);
        assert_eq!(
            outcome_of(&after, CheckName::FilesInUse),
            CheckOutcome::Pass
        );
    }
}
