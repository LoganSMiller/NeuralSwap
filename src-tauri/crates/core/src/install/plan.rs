//! Deriving an install plan.
//!
//! This is a pure function, and that is the point. Everything a user is shown
//! before they agree to an install - every path, every version transition,
//! every warning - is decided here, from data, with no filesystem access. So
//! the dry run and the real run cannot disagree about what is about to happen:
//! the real run is handed this same structure and does exactly what it says.
//!
//! Upstream decided as it copied. A file was inspected, judged and overwritten
//! in one pass, which meant the only way to find out what an install would do
//! was to let it happen, and a refusal half-way left the folder in a state
//! nobody had described.
//!
//! The shape checks here are lexical only. Proving a path really stays inside
//! the game folder needs the filesystem - a junction is not visible in a
//! string - so that check lives in `apply`, where the write happens.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::error::{fail, Code, Result};
use crate::install::version::{relate, VersionRelation};
use crate::scan::folder::RuntimeKind;

/// For now the one route: drop the DLLs beside the executable that loads them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Route {
    NativeDll,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageFile {
    pub name: String,
    pub kind: RuntimeKind,
    pub version: Option<String>,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PresentFile {
    pub rel: String,
    pub kind: RuntimeKind,
    pub version: Option<String>,
    pub size: u64,
    pub sha256: String,
    /// Recorded in our own install manifest, so we know we put it there.
    pub managed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanInput {
    pub route: Route,
    /// Directory inside the game to install into. Empty string is the game root.
    pub install_dir: String,
    pub present: Vec<PresentFile>,
    pub pkg: Vec<PackageFile>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StepAction {
    Create,
    Replace,
    Skip,
}

/// Why a step does what it does. Stable machine strings: the UI translates
/// them and the vectors assert them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StepReason {
    NewFile,
    Identical,
    Upgrade,
    Downgrade,
    SameVersionDifferentBytes,
    VersionUnknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Step {
    pub rel: String,
    pub action: StepAction,
    pub reason: StepReason,
    pub kind: RuntimeKind,
    pub from_version: Option<String>,
    pub to_version: Option<String>,
    /// Bytes the new file occupies. Zero for a skip.
    pub write_bytes: u64,
    /// Bytes that must be copied aside first. Zero unless something is replaced.
    pub backup_bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WarningCode {
    Downgrade,
    ReplacesUnmanagedFile,
    AddsKindNotPresent,
    MixedVersionsAfterInstall,
    NothingToDo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Warning {
    pub code: WarningCode,
    pub rels: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    pub route: Route,
    pub install_dir: String,
    pub steps: Vec<Step>,
    pub warnings: Vec<Warning>,
    pub write_bytes: u64,
    pub backup_bytes: u64,
    /// Steps that actually change the folder. Zero means already installed.
    pub changes: usize,
}

const RESERVED_STEMS: [&str; 4] = ["con", "prn", "aux", "nul"];

fn is_reserved(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).to_ascii_lowercase();
    if RESERVED_STEMS.contains(&stem.as_str()) {
        return true;
    }
    for prefix in ["com", "lpt"] {
        if let Some(rest) = stem.strip_prefix(prefix) {
            if rest.len() == 1 && rest.as_bytes().first().is_some_and(u8::is_ascii_digit) {
                return true;
            }
        }
    }
    false
}

fn is_separator(character: char) -> bool {
    character == '/' || character == '\\'
}

/// A package entry must be a plain file name. Anything with a separator in it
/// is either a package we do not understand or an attempt to write outside the
/// install directory, and neither is worth guessing about.
fn assert_plain_file_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return fail(Code::PackageInvalid, "package entry has an empty name");
    }
    if name.contains(is_separator) {
        return fail(
            Code::PackageInvalid,
            format!("package entry is not a plain file name: {name}"),
        );
    }
    if name.contains(':') {
        return fail(
            Code::PackageInvalid,
            format!("colon in package entry: {name}"),
        );
    }
    if name.contains('\0') {
        return fail(Code::PackageInvalid, "NUL byte in package entry");
    }
    if name == "." || name == ".." {
        return fail(
            Code::PackageInvalid,
            format!("package entry is a dot name: {name}"),
        );
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return fail(
            Code::PackageInvalid,
            format!("trailing dot or space in package entry: {name}"),
        );
    }
    if is_reserved(name) {
        return fail(
            Code::PackageInvalid,
            format!("DOS device name in package: {name}"),
        );
    }
    Ok(())
}

/// Forward slashes, so a plan reads the same however the scanner spelled it.
fn join_rel(dir: &str, name: &str) -> String {
    let clean = dir.trim_end_matches(is_separator);
    if clean.is_empty() {
        name.to_owned()
    } else {
        format!("{}/{name}", clean.replace('\\', "/"))
    }
}

/// Comparison key. Windows filesystems are case-insensitive; treat them so.
fn rel_key(rel: &str) -> String {
    rel.replace('\\', "/").to_lowercase()
}

fn parent_key(rel: &str) -> String {
    let key = rel_key(rel);
    match key.rfind('/') {
        Some(cut) => key[..cut].to_owned(),
        None => String::new(),
    }
}

/// The install directory as a comparison key, so it can be matched against
/// `parent_key` of a scanned file. Trailing separators and casing vary.
fn dir_key_of(install_dir: &str) -> String {
    rel_key(install_dir).trim_end_matches('/').to_owned()
}

fn decide(pkg: &PackageFile, present: Option<&PresentFile>) -> (StepAction, StepReason) {
    let Some(present) = present else {
        return (StepAction::Create, StepReason::NewFile);
    };
    // Byte equality first: it is the only comparison that is certainly right,
    // and it is what makes re-running an install a no-op instead of a rewrite.
    if present.sha256 == pkg.sha256 {
        return (StepAction::Skip, StepReason::Identical);
    }
    match relate(pkg.version.as_deref(), present.version.as_deref()) {
        VersionRelation::Newer => (StepAction::Replace, StepReason::Upgrade),
        VersionRelation::Older => (StepAction::Replace, StepReason::Downgrade),
        // Same version, different bytes. Somebody has already swapped this
        // file, or it was built differently. Worth replacing, worth backing up.
        VersionRelation::Same => (StepAction::Replace, StepReason::SameVersionDifferentBytes),
        VersionRelation::Unknown => (StepAction::Replace, StepReason::VersionUnknown),
    }
}

/// What each runtime version in the install directory will be once the plan has
/// run. Compared per kind, because DLSS and Streamline number independently -
/// `310.8.0.0` beside `2.13.0.0` is correct, and flagging it would be noise.
fn mixed_versions(input: &PlanInput, steps: &[Step], dir_key: &str) -> Vec<String> {
    let touched: BTreeSet<String> = steps.iter().map(|step| rel_key(&step.rel)).collect();
    // Ordered maps so the walk is deterministic without relying on the final
    // sort to hide a hash order.
    let mut by_kind: BTreeMap<RuntimeKind, BTreeMap<String, Vec<String>>> = BTreeMap::new();

    let mut note = |kind: RuntimeKind, version: Option<&str>, rel: &str| {
        let Some(version) = version else { return };
        by_kind
            .entry(kind)
            .or_default()
            .entry(version.to_owned())
            .or_default()
            .push(rel.to_owned());
    };

    for step in steps {
        let version = if step.action == StepAction::Skip {
            step.from_version.as_deref()
        } else {
            step.to_version.as_deref()
        };
        note(step.kind, version, &step.rel);
    }
    // Files already in the folder that the package says nothing about. These
    // are the ones left behind at an old version that then crash the game.
    for file in &input.present {
        if parent_key(&file.rel) != dir_key || touched.contains(&rel_key(&file.rel)) {
            continue;
        }
        note(file.kind, file.version.as_deref(), &file.rel);
    }

    let mut offenders: Vec<String> = Vec::new();
    for versions in by_kind.values() {
        if versions.len() < 2 {
            continue;
        }
        for rels in versions.values() {
            offenders.extend(rels.iter().cloned());
        }
    }
    offenders.sort();
    offenders
}

pub fn build_plan(input: &PlanInput) -> Result<Plan> {
    if input.pkg.is_empty() {
        return fail(Code::PackageInvalid, "package contains no runtime files");
    }

    // Sizes are `u64` here rather than a signed type, so the reference's
    // negative-size guard has no counterpart: a negative number fails to
    // deserialise before it can reach this function.
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for file in &input.pkg {
        assert_plain_file_name(&file.name)?;
        if !seen.insert(file.name.to_lowercase()) {
            return fail(
                Code::PackageInvalid,
                format!("duplicate package entry: {}", file.name),
            );
        }
    }

    let mut by_rel: BTreeMap<String, &PresentFile> = BTreeMap::new();
    for file in &input.present {
        by_rel.insert(rel_key(&file.rel), file);
    }

    let dir_key = dir_key_of(&input.install_dir);
    let mut kinds_present_in_dir: BTreeSet<RuntimeKind> = BTreeSet::new();
    for file in &input.present {
        if parent_key(&file.rel) == dir_key {
            kinds_present_in_dir.insert(file.kind);
        }
    }

    let mut steps: Vec<Step> = Vec::with_capacity(input.pkg.len());
    for file in &input.pkg {
        let rel = join_rel(&input.install_dir, &file.name);
        let present = by_rel.get(&rel_key(&rel)).copied();
        let (action, reason) = decide(file, present);
        steps.push(Step {
            rel,
            action,
            reason,
            kind: file.kind,
            from_version: present.and_then(|found| found.version.clone()),
            to_version: file.version.clone(),
            write_bytes: if action == StepAction::Skip {
                0
            } else {
                file.size
            },
            backup_bytes: if action == StepAction::Replace {
                present.map_or(0, |found| found.size)
            } else {
                0
            },
            sha256: file.sha256.clone(),
        });
    }
    steps.sort_by_key(|step| rel_key(&step.rel));

    let collect = |test: &dyn Fn(&Step) -> bool| -> Vec<String> {
        steps
            .iter()
            .filter(|step| test(step))
            .map(|step| step.rel.clone())
            .collect()
    };

    let mut warnings: Vec<Warning> = Vec::new();
    let downgrades = collect(&|step| step.reason == StepReason::Downgrade);
    if !downgrades.is_empty() {
        warnings.push(Warning {
            code: WarningCode::Downgrade,
            rels: downgrades,
        });
    }

    // A replacement of something we did not install is the case worth stating
    // plainly: it may be the game's own file, or a swap the user did by hand
    // and has forgotten. The backup makes it reversible either way, but they
    // should be told before it happens, not after.
    let unmanaged = collect(&|step| {
        step.action == StepAction::Replace
            && by_rel
                .get(&rel_key(&step.rel))
                .is_none_or(|found| !found.managed)
    });
    if !unmanaged.is_empty() {
        warnings.push(Warning {
            code: WarningCode::ReplacesUnmanagedFile,
            rels: unmanaged,
        });
    }

    let novel = collect(&|step| {
        step.action == StepAction::Create && !kinds_present_in_dir.contains(&step.kind)
    });
    if !novel.is_empty() {
        warnings.push(Warning {
            code: WarningCode::AddsKindNotPresent,
            rels: novel,
        });
    }

    let mixed = mixed_versions(input, &steps, &dir_key);
    if !mixed.is_empty() {
        warnings.push(Warning {
            code: WarningCode::MixedVersionsAfterInstall,
            rels: mixed,
        });
    }

    let changes = steps
        .iter()
        .filter(|step| step.action != StepAction::Skip)
        .count();
    if changes == 0 {
        warnings.push(Warning {
            code: WarningCode::NothingToDo,
            rels: Vec::new(),
        });
    }

    Ok(Plan {
        route: input.route,
        install_dir: input.install_dir.clone(),
        write_bytes: steps.iter().map(|step| step.write_bytes).sum(),
        backup_bytes: steps.iter().map(|step| step.backup_bytes).sum(),
        steps,
        warnings,
        changes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkg_file(name: &str) -> PackageFile {
        PackageFile {
            name: name.to_owned(),
            kind: RuntimeKind::Dlss,
            version: Some("310.8.0.0".to_owned()),
            size: 1000,
            sha256: "new".to_owned(),
        }
    }

    fn present_file(rel: &str, version: &str, sha: &str) -> PresentFile {
        PresentFile {
            rel: rel.to_owned(),
            kind: RuntimeKind::Dlss,
            version: Some(version.to_owned()),
            size: 900,
            sha256: sha.to_owned(),
            managed: false,
        }
    }

    fn input(present: Vec<PresentFile>, pkg: Vec<PackageFile>) -> PlanInput {
        PlanInput {
            route: Route::NativeDll,
            install_dir: "bin/x64".to_owned(),
            present,
            pkg,
        }
    }

    fn codes(plan: &Plan) -> Vec<WarningCode> {
        plan.warnings.iter().map(|warning| warning.code).collect()
    }

    #[test]
    fn a_missing_file_is_created_without_a_backup() {
        let plan = build_plan(&input(vec![], vec![pkg_file("nvngx_dlss.dll")])).expect("plan");
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].action, StepAction::Create);
        assert_eq!(plan.steps[0].reason, StepReason::NewFile);
        assert_eq!(plan.backup_bytes, 0);
        assert_eq!(plan.changes, 1);
    }

    #[test]
    fn identical_bytes_are_skipped() {
        let mut present = present_file("bin/x64/nvngx_dlss.dll", "310.8.0.0", "new");
        present.size = 1000;
        let plan =
            build_plan(&input(vec![present], vec![pkg_file("nvngx_dlss.dll")])).expect("plan");
        assert_eq!(plan.steps[0].action, StepAction::Skip);
        assert_eq!(plan.changes, 0);
        assert!(codes(&plan).contains(&WarningCode::NothingToDo));
    }

    #[test]
    fn the_install_directory_is_matched_case_insensitively() {
        // The scanner reports what the directory entry said; a manifest may
        // disagree about capitalisation. On NTFS they are the same folder.
        let mut request = input(
            vec![present_file("bin\\x64\\NVNGX_DLSS.DLL", "310.1.0.0", "old")],
            vec![pkg_file("nvngx_dlss.dll")],
        );
        request.install_dir = "Bin/X64".to_owned();
        let plan = build_plan(&request).expect("plan");
        assert_eq!(plan.steps[0].action, StepAction::Replace);
        assert_eq!(plan.steps[0].reason, StepReason::Upgrade);
    }

    #[test]
    fn a_package_entry_that_is_not_a_plain_file_name_is_refused() {
        for name in [
            "../escape.dll",
            "sub/nested.dll",
            "sub\\nested.dll",
            "C:\\Windows\\evil.dll",
            "x.dll:hidden",
            "evil.dll.",
            "evil.dll ",
            "NUL",
            "COM1.dll",
            "",
            "x\0.dll",
        ] {
            let outcome = build_plan(&input(vec![], vec![pkg_file(name)]));
            assert_eq!(
                outcome.err().map(|error| error.code),
                Some(Code::PackageInvalid),
                "{name:?} should be refused"
            );
        }
        // A name that merely contains a reserved stem is an ordinary file.
        assert!(build_plan(&input(vec![], vec![pkg_file("nullify.dll")])).is_ok());
    }

    #[test]
    fn kinds_number_independently() {
        // 310.8.0.0 beside 2.13.0.0 is correct. Comparing across kinds would
        // fire this warning on every healthy install.
        let mut streamline = present_file("bin/x64/sl.dlss.dll", "2.13.0.0", "sl");
        streamline.kind = RuntimeKind::Streamline;
        let plan = build_plan(&input(
            vec![
                present_file("bin/x64/nvngx_dlss.dll", "310.8.0.0", "old"),
                streamline,
            ],
            vec![pkg_file("nvngx_dlss.dll")],
        ))
        .expect("plan");
        assert!(!codes(&plan).contains(&WarningCode::MixedVersionsAfterInstall));
    }

    #[test]
    fn a_sibling_left_at_the_old_version_is_flagged() {
        let plan = build_plan(&input(
            vec![
                present_file("bin/x64/nvngx_dlss.dll", "310.1.0.0", "old"),
                present_file("bin/x64/nvngx_dlssg.dll", "310.1.0.0", "oldg"),
            ],
            vec![pkg_file("nvngx_dlss.dll")],
        ))
        .expect("plan");
        let mixed = plan
            .warnings
            .iter()
            .find(|warning| warning.code == WarningCode::MixedVersionsAfterInstall)
            .expect("a mixed-version warning");
        assert_eq!(
            mixed.rels,
            vec!["bin/x64/nvngx_dlss.dll", "bin/x64/nvngx_dlssg.dll"]
        );
    }

    #[test]
    fn steps_come_out_in_a_stable_order() {
        let plan = build_plan(&input(
            vec![],
            vec![
                pkg_file("sl.dlss.dll"),
                pkg_file("nvngx_dlss.dll"),
                pkg_file("sl.common.dll"),
            ],
        ))
        .expect("plan");
        assert_eq!(
            plan.steps
                .iter()
                .map(|step| step.rel.as_str())
                .collect::<Vec<_>>(),
            vec![
                "bin/x64/nvngx_dlss.dll",
                "bin/x64/sl.common.dll",
                "bin/x64/sl.dlss.dll"
            ]
        );
    }

    #[test]
    fn an_empty_or_duplicated_package_is_refused() {
        assert_eq!(
            build_plan(&input(vec![], vec![])).err().map(|e| e.code),
            Some(Code::PackageInvalid)
        );
        let dupe = build_plan(&input(
            vec![],
            vec![pkg_file("nvngx_dlss.dll"), pkg_file("NVNGX_DLSS.DLL")],
        ));
        assert_eq!(dupe.err().map(|e| e.code), Some(Code::PackageInvalid));
    }
}
