//! Registering a Vulkan implicit layer, and knowing when to take it away.
//!
//! Every other install in this project writes files into one game folder, and
//! undoing it means putting those files back. This one does not. A Vulkan
//! layer is a value under `HKCU\Software\Khronos\Vulkan\ImplicitLayers` naming
//! a manifest's absolute path, and the Vulkan loader applies **every**
//! registered implicit layer to **every** Vulkan application on the account.
//!
//! So three things are true here that are true nowhere else in the installer:
//!
//! 1. **Installing "into a game" is not what it sounds like.** The effect is
//!    machine-wide. A user who asked to change one game has changed all of
//!    them, and has to be told so before it happens.
//! 2. **Undo cannot be "remove what we added".** Two games can want the same
//!    layer. Removing the registration when the first is uninstalled breaks
//!    the second, silently, and the user has no reason to connect the two.
//!    So the shared directory carries a list of the games that asked, and the
//!    registration only goes when the list empties. DLSS5-Swapper's
//!    `vulkan-layer.js` keeps exactly such an `installs.json`, and this is the
//!    same design.
//! 3. **A registration we did not make is not ours to take over.** If the user
//!    already runs ReShade as a Vulkan layer, or another tool does, replacing
//!    or removing it would change a setup we know nothing about. It is
//!    reported and left alone.
//!
//! # The registry is a trait
//!
//! For the same reason the component fetcher is: this crate is host-agnostic,
//! and every rule worth having here can then be tested without touching a real
//! machine's registry. A fake can present a foreign registration, a read that
//! fails, or a write that fails, and none of those are reproducible against
//! `HKCU`.
//!
//! It also means the tests do not modify the developer's own Vulkan setup,
//! which is not a thing a test suite should be able to do by accident.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Code, Result};
use crate::fsx::atomic::{read_to_string_or_none, write_json_atomic};

/// Where the Vulkan loader looks for implicit layers, per user.
///
/// `HKCU` rather than `HKLM`: it needs no administrator, and it scopes the
/// change to the person who asked for it rather than to everyone with an
/// account on the machine.
pub const REGISTRY_KEY: &str = r"Software\Khronos\Vulkan\ImplicitLayers";

/// The registry, as this module needs it.
pub trait LayerRegistry {
    /// Every implicit-layer value currently registered, by name.
    ///
    /// The name is the manifest's absolute path; the data is a `DWORD` where
    /// zero means enabled. Values that are not layer manifests, or that are
    /// disabled, are the host's business to filter or not - this module only
    /// asks whether a path is listed.
    fn values(&self) -> Result<Vec<String>>;

    fn add(&self, value: &str) -> Result<()>;

    fn remove(&self, value: &str) -> Result<()>;
}

/// What staging the layer's files did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "outcome")]
pub enum Staged {
    /// Files were written into the shared directory.
    Written { files: Vec<String> },
    /// Everything was already exactly right, so nothing was touched.
    ///
    /// Worth distinguishing: the shared directory is used by every game that
    /// wants the layer, and rewriting it while another game is running would
    /// replace a DLL out from under a live process.
    AlreadyCurrent,
}

/// Put the layer's files in the shared directory, ready to be registered.
///
/// `from` is the component's extracted contents; `shared_dir` is the one
/// directory for the machine.
///
/// # Which files
///
/// A 64-bit game needs the 64-bit pair. A 32-bit game needs **both**, and the
/// reason is not obvious: the registry list is per account, not per
/// application, and every entry names one manifest. Both manifests declare the
/// same layer name - `VK_LAYER_reshade`, verified in the 6.8.0 installer - so
/// they are not two layers but two architectures of one, and the Vulkan loader
/// picks the matching build. Register only the 32-bit one and every 64-bit
/// application on the account has a layer it cannot load.
///
/// DLSS5-Swapper stages exactly this set for exactly this reason.
///
/// # Why the files are checked before they are copied
///
/// This directory gets registered globally. A wrong file here is not one
/// broken game, it is every Vulkan application on the account trying to load
/// something that cannot work - so each DLL is verified to be ReShade, to be
/// the add-on build, and to be the architecture its own name claims, before it
/// is allowed in.
pub fn stage(from: &Path, shared_dir: &Path, bitness: u8) -> Result<Staged> {
    let wanted: &[u8] = if bitness == 32 { &[64, 32] } else { &[64] };

    // Everything is checked before anything is written, the same discipline
    // the archive extractor and the installer use: a refusal half way through
    // leaves a directory nobody described - and this one is shared.
    //
    // The intended *contents* are resolved here rather than the sources,
    // because the manifests are rewritten on the way in. Comparing a target
    // against its source would never match for those, and the directory would
    // be rewritten on every install - replacing a DLL out from under whatever
    // game happened to be running off it.
    let mut planned: Vec<(PathBuf, Vec<u8>, String)> = Vec::new();
    for &each in wanted {
        let dll = format!("ReShade{each}.dll");
        let json = format!("ReShade{each}.json");
        let source_dll = from.join(&dll);
        let source_json = from.join(&json);

        for path in [&source_dll, &source_json] {
            if !path.is_file() {
                return crate::error::fail(
                    Code::PackageInvalid,
                    format!("{} is not in this package", path.display()),
                );
            }
        }

        let check = crate::scan::footprints::inspect_injector(&source_dll);
        if !check.usable_for(each) {
            return crate::error::fail(
                Code::PackageInvalid,
                format!(
                    "{dll} is not a {each}-bit ReShade with add-on support \
                     (reshade={}, add-ons={}, bitness={:?}) - registering it would \
                     affect every Vulkan program on this account",
                    check.is_reshade, check.has_addon_support, check.bitness
                ),
            );
        }

        let bytes = std::fs::read(&source_dll).map_err(|error| {
            crate::Error::new(
                Code::PackageInvalid,
                format!("could not read {}: {error}", source_dll.display()),
            )
        })?;
        planned.push((shared_dir.join(&dll), bytes, dll.clone()));
        planned.push((
            shared_dir.join(&json),
            manifest_bytes(&source_json, &dll)?,
            json,
        ));
    }

    // Already right? Then leave it alone. Another game may be running off
    // these very files.
    if planned
        .iter()
        .all(|(target, intended, _)| std::fs::read(target).is_ok_and(|found| &found == intended))
    {
        return Ok(Staged::AlreadyCurrent);
    }

    std::fs::create_dir_all(shared_dir).map_err(|error| {
        crate::Error::new(
            Code::StateUnwritable,
            format!("could not create {}: {error}", shared_dir.display()),
        )
    })?;

    let mut written = Vec::new();
    for (target, intended, name) in &planned {
        crate::fsx::atomic::write_atomic(target, intended)?;
        written.push(name.clone());
    }
    Ok(Staged::Written { files: written })
}

/// The manifest as it should be written: the vendor's, with `library_path`
/// naming its own sibling.
///
/// The shipped value is already `.\ReShade64.dll` - relative to the manifest,
/// so a same-directory copy is correct as it stands. It is set anyway, and
/// only that one field, so the copy is self-consistent whatever the source
/// said. A manifest that points at a DLL which is not beside it is a global
/// registration that cannot load, and that is not a thing to leave to trust.
///
/// Every other *value* is carried across: the layer name, the API version, the
/// device extensions and the disable switch are ReShade's business, not ours.
///
/// The key **order** is not preserved - the document is parsed and
/// re-serialised, and that sorts object keys. JSON objects are unordered by
/// definition and the Vulkan loader parses the file rather than matching it,
/// so this changes nothing that reads it. Worth stating rather than implying
/// the bytes come through unchanged, because they do not: a digest taken of
/// the vendor's file will not match the staged one.
fn manifest_bytes(source: &Path, dll: &str) -> Result<Vec<u8>> {
    let text = std::fs::read_to_string(source).map_err(|error| {
        crate::Error::new(
            Code::PackageInvalid,
            format!("could not read {}: {error}", source.display()),
        )
    })?;
    let mut parsed: serde_json::Value = serde_json::from_str(&text).map_err(|error| {
        crate::Error::new(
            Code::PackageInvalid,
            format!("{} is not JSON: {error}", source.display()),
        )
    })?;

    let Some(layer) = parsed
        .get_mut("layer")
        .and_then(|found| found.as_object_mut())
    else {
        return crate::error::fail(
            Code::PackageInvalid,
            format!("{} has no \"layer\" object", source.display()),
        );
    };
    layer.insert(
        "library_path".to_owned(),
        serde_json::Value::String(format!(".\\{dll}")),
    );

    let mut bytes = serde_json::to_vec_pretty(&parsed).map_err(|error| {
        crate::Error::new(
            Code::StateUnwritable,
            format!("could not serialise {}: {error}", source.display()),
        )
    })?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// A registry with nothing in it, which refuses to be written.
///
/// For the two cases where there is genuinely nothing to talk to: a build for
/// a platform that has no such registry, and a caller that is installing files
/// only. Reads answer "no layers"; a write is a error rather than a silent
/// success, because a caller that reaches one has asked for something this
/// cannot do.
pub struct NoRegistry;

impl LayerRegistry for NoRegistry {
    fn values(&self) -> Result<Vec<String>> {
        Ok(Vec::new())
    }
    fn add(&self, value: &str) -> Result<()> {
        crate::error::fail(
            Code::BadRequest,
            format!("no Vulkan layer registry is available to register {value}"),
        )
    }
    fn remove(&self, _value: &str) -> Result<()> {
        // Nothing is registered, so it is already in the state asked for.
        // Undo has to be idempotent: recovery runs it again after a crash.
        Ok(())
    }
}

/// What happened, in terms the user needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "outcome")]
pub enum Registered {
    /// The layer was registered, by us, now.
    Added {
        value: String,
        /// True when this is the first game to ask.
        first: bool,
    },
    /// Already registered by us; this game joined the list.
    AlreadyOurs { value: String, games: usize },
    /// Something else holds a layer registration. Left untouched.
    ///
    /// Not an error: the user may well want their own ReShade layer, and it
    /// may already do the job. But it is not ours, so we neither replace it
    /// nor claim credit for it, and the install has to say which.
    Foreign { value: String },
}

/// What happened when undoing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "outcome")]
pub enum Deregistered {
    /// The last game wanted it gone, so it went.
    Removed { value: String },
    /// This game was removed from the list; others still want the layer.
    StillWanted { value: String, games: usize },
    /// Nothing of ours was registered, so there was nothing to undo.
    NothingOfOurs,
}

/// The games that have asked for the layer.
///
/// A plain file beside the manifest rather than anything clever. It is the
/// reference count, and it has to survive a crash and be readable by a human
/// wondering why a registry entry exists.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Users {
    version: u32,
    games: Vec<String>,
}

const USERS_FILE: &str = "layer-users.json";
const USERS_VERSION: u32 = 1;

/// Case-folded and separator-normalised, because NTFS treats `D:\Games\X` and
/// `d:/games/x` as one folder and two installs into it must count as one.
fn key_of(game_dir: &Path) -> String {
    game_dir.to_string_lossy().replace('\\', "/").to_lowercase()
}

fn users_path(shared_dir: &Path) -> PathBuf {
    shared_dir.join(USERS_FILE)
}

fn read_users(shared_dir: &Path) -> Result<Users> {
    let Some(text) = read_to_string_or_none(&users_path(shared_dir))? else {
        return Ok(Users {
            version: USERS_VERSION,
            games: Vec::new(),
        });
    };
    // A list we cannot read is treated as empty rather than as an error. The
    // consequence of getting this wrong in the cautious direction is a layer
    // that outlives its last user, which is untidy; in the other direction it
    // is a layer removed from under a game that still needs it.
    Ok(serde_json::from_str(&text).unwrap_or(Users {
        version: USERS_VERSION,
        games: Vec::new(),
    }))
}

fn write_users(shared_dir: &Path, users: &Users) -> Result<()> {
    std::fs::create_dir_all(shared_dir).map_err(|error| {
        crate::Error::new(
            Code::StateUnwritable,
            format!("could not create {}: {error}", shared_dir.display()),
        )
    })?;
    write_json_atomic(&users_path(shared_dir), users)
}

/// The registry value name for a manifest: its absolute path.
fn value_for(shared_dir: &Path, manifest: &str) -> String {
    shared_dir.join(manifest).to_string_lossy().into_owned()
}

/// Is this registered value one of ours?
fn is_ours(value: &str, ours: &str) -> bool {
    value.replace('\\', "/").to_lowercase() == ours.replace('\\', "/").to_lowercase()
}

/// Does this value look like some other ReShade layer registration?
///
/// Matched on the file name rather than the path, because the question is
/// whether *a* ReShade layer is already doing this job - wherever the user or
/// another tool put it.
fn is_a_reshade_layer(value: &str) -> bool {
    let lower = value.replace('\\', "/").to_lowercase();
    lower
        .rsplit('/')
        .next()
        .is_some_and(|name| name.starts_with("reshade") && name.ends_with(".json"))
}

/// Ensure the layer is registered and that `game_dir` is counted as wanting it.
///
/// `shared_dir` is where the layer's files live - one directory for the whole
/// machine, not inside any game.
pub fn register(
    registry: &dyn LayerRegistry,
    shared_dir: &Path,
    manifest: &str,
    game_dir: &Path,
) -> Result<Registered> {
    let ours = value_for(shared_dir, manifest);
    let existing = registry.values()?;

    // Somebody else's ReShade layer. Not ours to replace, and quite possibly
    // doing the job already - but we cannot know, so we say what we found and
    // change nothing.
    if let Some(foreign) = existing
        .iter()
        .find(|value| is_a_reshade_layer(value) && !is_ours(value, &ours))
    {
        return Ok(Registered::Foreign {
            value: foreign.clone(),
        });
    }

    let already = existing.iter().any(|value| is_ours(value, &ours));

    // The list is updated before the registry write, so a crash between the
    // two leaves a game counted for a layer that is not registered. That is
    // the harmless direction: the next install registers it, and an undo
    // finds nothing to remove and says so. The other order would leave a
    // registration nothing admits to wanting.
    let mut users = read_users(shared_dir)?;
    let key = key_of(game_dir);
    let first = users.games.is_empty();
    if !users.games.iter().any(|item| item == &key) {
        users.games.push(key);
        write_users(shared_dir, &users)?;
    }

    if already {
        return Ok(Registered::AlreadyOurs {
            value: ours,
            games: users.games.len(),
        });
    }

    registry.add(&ours)?;
    Ok(Registered::Added { value: ours, first })
}

/// Stop counting `game_dir`, and deregister the layer if nothing else wants it.
pub fn deregister(
    registry: &dyn LayerRegistry,
    shared_dir: &Path,
    manifest: &str,
    game_dir: &Path,
) -> Result<Deregistered> {
    let ours = value_for(shared_dir, manifest);
    let mut users = read_users(shared_dir)?;
    let key = key_of(game_dir);
    users.games.retain(|item| item != &key);
    write_users(shared_dir, &users)?;

    if !users.games.is_empty() {
        return Ok(Deregistered::StillWanted {
            value: ours,
            games: users.games.len(),
        });
    }

    // Nothing wants it. Remove only a registration that is actually ours -
    // if the value is not there, or is somebody else's, there is nothing here
    // to undo.
    let existing = registry.values()?;
    if !existing.iter().any(|value| is_ours(value, &ours)) {
        return Ok(Deregistered::NothingOfOurs);
    }
    registry.remove(&ours)?;
    Ok(Deregistered::Removed { value: ours })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// A registry that lives in memory, so no test can touch a real one.
    #[derive(Default)]
    struct Fake {
        values: RefCell<Vec<String>>,
        fail_add: bool,
    }

    impl Fake {
        fn with(values: &[&str]) -> Self {
            Self {
                values: RefCell::new(values.iter().map(|item| (*item).to_owned()).collect()),
                fail_add: false,
            }
        }
    }

    impl LayerRegistry for Fake {
        fn values(&self) -> Result<Vec<String>> {
            Ok(self.values.borrow().clone())
        }
        fn add(&self, value: &str) -> Result<()> {
            if self.fail_add {
                return crate::error::fail(Code::StateUnwritable, "refused");
            }
            self.values.borrow_mut().push(value.to_owned());
            Ok(())
        }
        fn remove(&self, value: &str) -> Result<()> {
            self.values.borrow_mut().retain(|item| item != value);
            Ok(())
        }
    }

    fn shared() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    /// A package holding synthetic ReShade files.
    ///
    /// Synthetic rather than the real 5.5 MB DLLs: what `stage` cares about is
    /// the two marker strings and the PE machine field, and a fixture that
    /// carries exactly those says what the check depends on. The real binaries
    /// are verified against `inspect_injector` in `scan::footprints`.
    fn package(bits: &[u8]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        for &each in bits {
            // A minimal PE, far enough in for the reader to answer bitness.
            // Bitness comes from the optional header's magic - PE32 against
            // PE32+ - not from the COFF machine field, and the optional
            // header is only read if `SizeOfOptionalHeader` says it is there.
            // Leaving that zero was the first version's mistake.
            //
            // Written at explicit offsets rather than by appending, because
            // this is a structure and appending hides where things land.
            const PE_AT: usize = 0x80;
            let mut dll = vec![0_u8; 0x400];
            let put16 = |dll: &mut Vec<u8>, at: usize, value: u16| {
                dll[at..at + 2].copy_from_slice(&value.to_le_bytes());
            };

            dll[0..2].copy_from_slice(b"MZ");
            dll[0x3c..0x40].copy_from_slice(&(PE_AT as u32).to_le_bytes());
            dll[PE_AT..PE_AT + 4].copy_from_slice(b"PE\0\0");
            // COFF header: machine, section count, and the size of the
            // optional header that follows it.
            put16(
                &mut dll,
                PE_AT + 4,
                if each == 32 { 0x014c } else { 0x8664 },
            );
            put16(&mut dll, PE_AT + 6, 0);
            let optional_size: u16 = if each == 32 { 224 } else { 240 };
            put16(&mut dll, PE_AT + 20, optional_size);
            // The optional header's magic, which is what decides bitness.
            put16(
                &mut dll,
                PE_AT + 24,
                if each == 32 { 0x010b } else { 0x020b },
            );

            dll.extend_from_slice(b"ReShade");
            dll.extend_from_slice(b"Searching for add-ons");
            std::fs::write(dir.path().join(format!("ReShade{each}.dll")), &dll).expect("dll");

            // The manifest as shipped, including the sibling-relative path.
            let json = format!(
                r#"{{"file_format_version":"1.0.0","layer":{{"name":"VK_LAYER_reshade","type":"GLOBAL","library_path":".\\ReShade{each}.dll"}}}}"#
            );
            std::fs::write(dir.path().join(format!("ReShade{each}.json")), json).expect("json");
        }
        dir
    }

    #[test]
    fn a_sixty_four_bit_game_stages_one_pair() {
        let from = package(&[64, 32]);
        let shared = shared();
        let done = stage(from.path(), shared.path(), 64).expect("stage");

        match done {
            Staged::Written { files } => {
                assert_eq!(files, vec!["ReShade64.dll", "ReShade64.json"]);
            }
            other => panic!("expected a write, got {other:?}"),
        }
        assert!(shared.path().join("ReShade64.dll").is_file());
        assert!(!shared.path().join("ReShade32.dll").exists());
    }

    #[test]
    fn a_thirty_two_bit_game_stages_both_pairs() {
        // Not obvious, and the reason matters: the registry list is per
        // account and both manifests declare the same layer name, so they are
        // two architectures of one layer. Register only the 32-bit one and
        // every 64-bit application on the account has a layer it cannot load.
        let from = package(&[64, 32]);
        let shared = shared();
        let done = stage(from.path(), shared.path(), 32).expect("stage");

        match done {
            Staged::Written { files } => assert_eq!(
                files,
                vec![
                    "ReShade64.dll",
                    "ReShade64.json",
                    "ReShade32.dll",
                    "ReShade32.json"
                ]
            ),
            other => panic!("expected a write, got {other:?}"),
        }
    }

    #[test]
    fn staging_twice_writes_nothing_the_second_time() {
        // The shared directory is used by every game that wants the layer.
        // Rewriting it needlessly would replace a DLL out from under whatever
        // game happens to be running off it.
        //
        // This is the case an earlier version got wrong: it compared each
        // target against its *source*, and the manifests are rewritten on the
        // way in, so they never matched and the directory was rewritten every
        // time.
        let from = package(&[64]);
        let shared = shared();
        assert!(matches!(
            stage(from.path(), shared.path(), 64),
            Ok(Staged::Written { .. })
        ));
        assert_eq!(
            stage(from.path(), shared.path(), 64).expect("second"),
            Staged::AlreadyCurrent
        );
    }

    #[test]
    fn the_staged_manifest_points_at_its_own_sibling() {
        let from = package(&[64]);
        let shared = shared();
        stage(from.path(), shared.path(), 64).expect("stage");

        let text =
            std::fs::read_to_string(shared.path().join("ReShade64.json")).expect("read manifest");
        let parsed: serde_json::Value = serde_json::from_str(&text).expect("json");
        assert_eq!(parsed["layer"]["library_path"], ".\\ReShade64.dll");
        // And nothing else was invented or lost.
        assert_eq!(parsed["layer"]["name"], "VK_LAYER_reshade");
        assert_eq!(parsed["layer"]["type"], "GLOBAL");
        assert_eq!(parsed["file_format_version"], "1.0.0");
    }

    #[test]
    fn a_manifest_pointing_elsewhere_is_corrected() {
        // A manifest naming a DLL that is not beside it is a global
        // registration that cannot load. The one field is set so the copy is
        // self-consistent whatever the source said.
        let from = package(&[64]);
        std::fs::write(
            from.path().join("ReShade64.json"),
            r#"{"layer":{"name":"VK_LAYER_reshade","library_path":"C:\\somewhere\\else.dll"}}"#,
        )
        .expect("write");

        let shared = shared();
        stage(from.path(), shared.path(), 64).expect("stage");
        let text =
            std::fs::read_to_string(shared.path().join("ReShade64.json")).expect("read manifest");
        assert!(text.contains("ReShade64.dll"), "{text}");
        assert!(!text.contains("somewhere"), "{text}");
    }

    #[test]
    fn the_wrong_build_is_refused_before_anything_is_written() {
        // This directory gets registered globally. A wrong file here is not
        // one broken game, it is every Vulkan application on the account
        // trying to load something that cannot work.
        let from = package(&[64]);
        // Strip the add-on marker: plain ReShade, which loads no add-ons.
        let plain = std::fs::read(from.path().join("ReShade64.dll"))
            .expect("read")
            .into_iter()
            .collect::<Vec<u8>>();
        let cut = plain.len() - b"Searching for add-ons".len();
        std::fs::write(from.path().join("ReShade64.dll"), &plain[..cut]).expect("write");

        let shared = shared();
        let refused = stage(from.path(), shared.path(), 64).expect_err("plain build");
        assert_eq!(refused.code, Code::PackageInvalid);
        assert!(refused.detail.contains("add-on"), "{refused:?}");
        assert!(
            !shared.path().join("ReShade64.dll").exists(),
            "nothing may be written when the check fails"
        );
    }

    #[test]
    fn a_mismatched_architecture_is_refused() {
        // A 32-bit DLL wearing the 64-bit name. Registered globally, it would
        // be a layer no 64-bit application can load.
        let from = package(&[64, 32]);
        let thirty_two = std::fs::read(from.path().join("ReShade32.dll")).expect("read");
        std::fs::write(from.path().join("ReShade64.dll"), thirty_two).expect("write");

        let shared = shared();
        let refused = stage(from.path(), shared.path(), 64).expect_err("wrong architecture");
        assert_eq!(refused.code, Code::PackageInvalid);
    }

    #[test]
    fn a_package_missing_a_file_is_refused_rather_than_half_staged() {
        let from = package(&[64]);
        std::fs::remove_file(from.path().join("ReShade64.json")).expect("remove");
        let shared = shared();

        let refused = stage(from.path(), shared.path(), 64).expect_err("incomplete");
        assert_eq!(refused.code, Code::PackageInvalid);
        assert!(
            !shared.path().join("ReShade64.dll").exists(),
            "the DLL must not be staged without its manifest"
        );
    }

    #[test]
    fn the_first_game_registers_the_layer() {
        let dir = shared();
        let registry = Fake::default();
        let found = register(
            &registry,
            dir.path(),
            "ReShade64.json",
            Path::new("D:/Games/One"),
        )
        .expect("register");

        match found {
            Registered::Added { first, value } => {
                assert!(first);
                assert!(value.ends_with("ReShade64.json"), "{value}");
            }
            other => panic!("expected an add, got {other:?}"),
        }
        assert_eq!(registry.values.borrow().len(), 1);
    }

    #[test]
    fn a_second_game_joins_the_list_rather_than_registering_twice() {
        let dir = shared();
        let registry = Fake::default();
        register(
            &registry,
            dir.path(),
            "ReShade64.json",
            Path::new("D:/Games/One"),
        )
        .expect("first");
        let found = register(
            &registry,
            dir.path(),
            "ReShade64.json",
            Path::new("D:/Games/Two"),
        )
        .expect("second");

        assert!(matches!(found, Registered::AlreadyOurs { games: 2, .. }));
        assert_eq!(
            registry.values.borrow().len(),
            1,
            "one registration, however many games want it"
        );
    }

    #[test]
    fn undoing_one_game_does_not_break_another() {
        // The reason the reference count exists. Two games want the layer;
        // uninstalling from the first must not deregister it, or the second
        // silently stops working and nothing connects the two events.
        let dir = shared();
        let registry = Fake::default();
        for game in ["D:/Games/One", "D:/Games/Two"] {
            register(&registry, dir.path(), "ReShade64.json", Path::new(game)).expect("register");
        }

        let first = deregister(
            &registry,
            dir.path(),
            "ReShade64.json",
            Path::new("D:/Games/One"),
        )
        .expect("deregister");
        assert!(matches!(first, Deregistered::StillWanted { games: 1, .. }));
        assert_eq!(
            registry.values.borrow().len(),
            1,
            "the layer must survive while another game wants it"
        );

        let second = deregister(
            &registry,
            dir.path(),
            "ReShade64.json",
            Path::new("D:/Games/Two"),
        )
        .expect("deregister");
        assert!(matches!(second, Deregistered::Removed { .. }));
        assert!(registry.values.borrow().is_empty());
    }

    #[test]
    fn the_same_game_spelled_differently_counts_once() {
        // NTFS treats these as one folder, and a reference count that does not
        // would leave a layer registered forever.
        let dir = shared();
        let registry = Fake::default();
        register(
            &registry,
            dir.path(),
            "ReShade64.json",
            Path::new("D:\\Games\\One"),
        )
        .expect("register");
        let again = register(
            &registry,
            dir.path(),
            "ReShade64.json",
            Path::new("d:/games/one"),
        )
        .expect("register");

        assert!(matches!(again, Registered::AlreadyOurs { games: 1, .. }));

        let gone = deregister(
            &registry,
            dir.path(),
            "ReShade64.json",
            Path::new("D:/Games/ONE"),
        )
        .expect("deregister");
        assert!(matches!(gone, Deregistered::Removed { .. }));
    }

    #[test]
    fn somebody_elses_layer_is_reported_and_left_alone() {
        // The user may already run ReShade as a Vulkan layer, or another tool
        // may. Replacing it would change a setup we know nothing about, and
        // removing it later would be worse.
        let theirs = r"C:\Users\someone\ReShade\ReShade64.json";
        let registry = Fake::with(&[theirs]);
        let dir = shared();

        let found = register(
            &registry,
            dir.path(),
            "ReShade64.json",
            Path::new("D:/Games/One"),
        )
        .expect("register");

        match found {
            Registered::Foreign { value } => assert_eq!(value, theirs),
            other => panic!("expected a foreign registration, got {other:?}"),
        }
        assert_eq!(
            registry.values.borrow().as_slice(),
            &[theirs.to_owned()],
            "nothing of theirs may be touched"
        );
        // And no game was counted, so a later undo has nothing to remove.
        assert!(!users_path(dir.path()).exists());
    }

    #[test]
    fn an_unrelated_layer_is_not_mistaken_for_a_reshade_one() {
        // Plenty of things register implicit layers - overlays, capture tools,
        // the validation layers. Only another ReShade is a reason to stand
        // down.
        let registry = Fake::with(&[
            r"C:\Program Files\Steam\SteamOverlayVulkanLayer.json",
            r"C:\VulkanSDK\VkLayer_khronos_validation.json",
        ]);
        let dir = shared();

        let found = register(
            &registry,
            dir.path(),
            "ReShade64.json",
            Path::new("D:/Games/One"),
        )
        .expect("register");
        assert!(matches!(found, Registered::Added { .. }), "{found:?}");
    }

    #[test]
    fn undoing_something_never_registered_is_not_an_error() {
        // A crash between counting a game and writing the registry value
        // leaves exactly this state, and the undo has to cope with it rather
        // than report a failure the user cannot act on.
        let dir = shared();
        let registry = Fake::default();
        let found = deregister(
            &registry,
            dir.path(),
            "ReShade64.json",
            Path::new("D:/Games/One"),
        )
        .expect("deregister");
        assert_eq!(found, Deregistered::NothingOfOurs);
    }

    #[test]
    fn a_failed_registry_write_does_not_leave_a_silent_success() {
        let dir = shared();
        let registry = Fake {
            values: RefCell::new(Vec::new()),
            fail_add: true,
        };
        let refused = register(
            &registry,
            dir.path(),
            "ReShade64.json",
            Path::new("D:/Games/One"),
        )
        .expect_err("the write failed");
        assert_eq!(refused.code, Code::StateUnwritable);
    }

    #[test]
    fn an_unreadable_user_list_is_treated_as_empty_rather_than_fatal() {
        // The cautious direction. Reading this wrongly as "nobody wants it"
        // leaves a layer registered longer than needed, which is untidy;
        // reading it wrongly the other way removes a layer from under a game
        // that still needs it.
        let dir = shared();
        std::fs::write(users_path(dir.path()), b"{ this is not json").expect("write");
        let users = read_users(dir.path()).expect("read");
        assert!(users.games.is_empty());
    }
}
