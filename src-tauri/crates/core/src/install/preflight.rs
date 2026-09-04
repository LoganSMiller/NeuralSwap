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
    ];

    let ok = !checks
        .iter()
        .any(|check| check.outcome == CheckOutcome::Fail);
    Preflight { checks, ok }
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
    fn a_healthy_install_passes_everything() {
        let fixture = fixture(b"runtime bytes");
        let report = run(&fixture);
        assert!(report.ok, "{:?}", report.blockers());
        // Every check is reported, not just the failures - the user sees the
        // whole picture on one screen.
        assert_eq!(report.checks.len(), 8);
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
        assert_eq!(report.checks.len(), 8);
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
