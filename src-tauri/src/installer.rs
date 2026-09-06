use std::path::{Path, PathBuf};
use std::sync::Mutex;

use neuralswap_core::error::{Code, Error, Result};
use neuralswap_core::install::{
    apply, journal, layer, manifest, package, plan, preflight, restore, Integrity, Plan,
};
use neuralswap_core::jobs::{Cancel, KeyedLock};
use neuralswap_core::platform::gpu::Generation;

use crate::registry::WindowsRegistry;

/// Owns the three on-disk stores an install needs, and the locks around them.
///
/// Kept out of `commands` so the command layer stays a thin, validated
/// boundary: nothing here knows about Tauri, and the paths are decided once at
/// startup rather than reconstructed per call.
pub struct Installer {
    /// Bookkeeping for in-flight installs. Emptied as each one commits.
    journal_root: PathBuf,
    /// Displaced originals. Permanent - the manifest points into this.
    backup_root: PathBuf,
    /// One record per game of what we installed.
    manifest_root: PathBuf,
    /// Per-game, refuse rather than queue: two installs into one folder is a
    /// mistake to report, not a request to serialise.
    locks: KeyedLock,
    cancel: Mutex<Cancel>,
    /// Held rather than constructed at each call, so a test can supply one
    /// that lives in memory. Registering a Vulkan layer changes machine-wide
    /// state, and a test suite that could reach the real registry would be
    /// able to alter a developer's own Vulkan setup by accident.
    layers: Box<dyn layer::LayerRegistry + Send + Sync>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The lock key for a game folder. Case-folded and separator-normalised,
/// because NTFS treats `D:\Games\X` and `d:/games/x` as one folder and two
/// installs into it must contend.
fn key_of(game_dir: &Path) -> String {
    game_dir.to_string_lossy().replace('\\', "/").to_lowercase()
}

impl Installer {
    pub fn new(data_dir: &Path) -> Self {
        Self::with_registry(data_dir, Box::new(WindowsRegistry))
    }

    pub fn with_registry(
        data_dir: &Path,
        layers: Box<dyn layer::LayerRegistry + Send + Sync>,
    ) -> Self {
        Self {
            journal_root: data_dir.join("journal"),
            backup_root: data_dir.join("backups"),
            manifest_root: data_dir.join("installs"),
            locks: KeyedLock::new(),
            cancel: Mutex::new(Cancel::new()),
            layers,
        }
    }

    /// Deal with anything an interrupted install left behind.
    ///
    /// Run once at startup, before the window is usable, because the state it
    /// resolves is a half-changed game folder and the user should not be
    /// invited to install on top of one.
    pub fn recover_at_startup(&self) -> Vec<journal::RecoveryOutcome> {
        match journal::recover_all(&self.journal_root, self.layers.as_ref()) {
            Ok(outcomes) => outcomes,
            Err(error) => {
                // Not fatal: the app is still usable, and the journals are
                // still there for the next attempt.
                log::warn!("could not inspect install journals: {error}");
                Vec::new()
            }
        }
    }

    /// Derive the plan for installing `package_dir` into `game_dir`.
    ///
    /// Reads both sides fresh rather than trusting anything cached: the plan a
    /// user is about to approve has to describe the folder as it is now.
    pub fn plan(&self, game_dir: &Path, install_dir: &str, package_dir: &Path) -> Result<Plan> {
        let managed = manifest::load(&self.manifest_root, game_dir)?
            .map(|record| record.managed_rels())
            .unwrap_or_default();

        plan::build_plan(&plan::PlanInput {
            route: plan::Route::NativeDll,
            install_dir: install_dir.to_owned(),
            present: package::read_present(game_dir, install_dir, &managed)?,
            pkg: package::read_package(package_dir)?,
        })
    }

    /// Run the checks without installing anything.
    ///
    /// `requires` is the hardware generation the package states it needs.
    /// `None` until packages carry that claim, at which point the check stops
    /// reporting "no requirement stated" and starts enforcing one.
    pub fn preflight(
        &self,
        game_dir: &Path,
        plan: &Plan,
        package_dir: &Path,
        requires: Option<Generation>,
        anti_cheat_acknowledged: bool,
    ) -> preflight::Preflight {
        preflight::preflight(&preflight::Request {
            game_dir,
            plan,
            source_dir: package_dir,
            backup_dir: &self.backup_root,
            requires,
            anti_cheat_acknowledged,
        })
    }

    /// Install. Blocking, and holds the game's lock for the duration.
    pub fn apply(
        &self,
        game_dir: &Path,
        plan: &Plan,
        package_dir: &Path,
        anti_cheat_acknowledged: bool,
    ) -> Result<apply::Outcome> {
        let key = key_of(game_dir);
        let Some(_guard) = self.locks.try_acquire(&key) else {
            return Err(Error::new(
                Code::JobBusy,
                "an install is already running for this game",
            ));
        };
        // A fresh token per run: a cancel from a previous install must not
        // abort this one before it starts.
        let cancel = {
            let mut held = lock(&self.cancel);
            *held = Cancel::new();
            held.clone()
        };

        apply::apply(&apply::Request {
            game_dir,
            plan,
            source_dir: package_dir,
            journal_root: &self.journal_root,
            backup_root: &self.backup_root,
            manifest_root: &self.manifest_root,
            requires: None,
            anti_cheat_acknowledged,
            layers: self.layers.as_ref(),
            cancel: &cancel,
        })
    }

    /// What we installed here, and whether it is still what we wrote.
    ///
    /// `None` means nothing was installed by us - which is different from an
    /// install that has been clobbered, and the UI needs to say different
    /// things about the two.
    pub fn status(&self, game_dir: &Path) -> Result<Option<Integrity>> {
        Ok(manifest::load(&self.manifest_root, game_dir)?
            .as_ref()
            .map(manifest::verify))
    }

    pub fn restore_preview(&self, game_dir: &Path) -> Result<restore::Outcome> {
        restore::preview(&restore::Request {
            game_dir,
            manifest_root: &self.manifest_root,
            layers: self.layers.as_ref(),
            cancel: &Cancel::new(),
        })
    }

    pub fn restore(&self, game_dir: &Path) -> Result<restore::Outcome> {
        let key = key_of(game_dir);
        let Some(_guard) = self.locks.try_acquire(&key) else {
            return Err(Error::new(
                Code::JobBusy,
                "something is already running for this game",
            ));
        };
        let cancel = {
            let mut held = lock(&self.cancel);
            *held = Cancel::new();
            held.clone()
        };
        restore::restore(&restore::Request {
            game_dir,
            manifest_root: &self.manifest_root,
            layers: self.layers.as_ref(),
            cancel: &cancel,
        })
    }

    // The Vulkan layer half, exercised by the tests below but not yet reached
    // from a command. `apply` will call it once the layer delivery lands
    // there; until then this is scaffolding, and saying so is better than
    // inventing a command to make the lint quiet.
    #[allow(dead_code)]
    /// Where the Vulkan layer's files live.
    ///
    /// One directory for the whole machine, deliberately not inside any game.
    /// A registered implicit layer is named by the absolute path of its
    /// manifest and applies to every Vulkan application on the account, so
    /// keeping a copy per game would mean several registrations all doing the
    /// same job and no clear answer to which one to remove.
    fn layer_dir(&self) -> PathBuf {
        self.backup_root
            .parent()
            .unwrap_or(&self.backup_root)
            .join("vulkan-layer")
    }

    /// Register the Vulkan layer on this account, counting `game_dir` as
    /// wanting it.
    ///
    /// Machine-wide. Every Vulkan application on this account is affected, and
    /// the caller is expected to have said so before getting here - see
    /// [`neuralswap_core::install::placement::Delivery::VulkanLayer`].
    #[allow(dead_code)]
    pub fn register_vulkan_layer(
        &self,
        game_dir: &Path,
        manifest: &str,
    ) -> Result<layer::Registered> {
        layer::register(self.layers.as_ref(), &self.layer_dir(), manifest, game_dir)
    }

    /// Stop counting `game_dir`, and deregister the layer if nothing else
    /// wants it.
    #[allow(dead_code)]
    pub fn deregister_vulkan_layer(
        &self,
        game_dir: &Path,
        manifest: &str,
    ) -> Result<layer::Deregistered> {
        layer::deregister(self.layers.as_ref(), &self.layer_dir(), manifest, game_dir)
    }

    /// Ask the running install to stop at the next file boundary. It will roll
    /// back what it has done.
    pub fn cancel(&self) {
        lock(&self.cancel).cancel();
    }

    pub fn is_busy(&self, game_dir: &Path) -> bool {
        self.locks.is_busy(&key_of(game_dir))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lock_key_folds_case_and_separators() {
        assert_eq!(
            key_of(Path::new("D:\\Games\\Cyberpunk 2077")),
            key_of(Path::new("d:/games/cyberpunk 2077"))
        );
        assert_ne!(
            key_of(Path::new("D:\\Games\\One")),
            key_of(Path::new("D:\\Games\\Two"))
        );
    }

    /// A registry that lives in memory. No test may reach the real one:
    /// registering a Vulkan layer is machine-wide state, and a suite that
    /// could write it would be able to change a developer's own setup.
    #[derive(Default)]
    struct FakeRegistry {
        values: std::sync::Mutex<Vec<String>>,
    }

    impl layer::LayerRegistry for FakeRegistry {
        fn values(&self) -> Result<Vec<String>> {
            Ok(lock(&self.values).clone())
        }
        fn add(&self, value: &str) -> Result<()> {
            lock(&self.values).push(value.to_owned());
            Ok(())
        }
        fn remove(&self, value: &str) -> Result<()> {
            lock(&self.values).retain(|item| item != value);
            Ok(())
        }
    }

    #[test]
    fn the_vulkan_layer_lives_outside_every_game() {
        // A registered implicit layer is named by its manifest's absolute
        // path and applies to every Vulkan application on the account. One
        // copy per game would mean several registrations doing the same job
        // and no clear answer to which to remove.
        let dir = tempfile::tempdir().expect("tempdir");
        let installer = Installer::new(dir.path());
        let layer_dir = installer.layer_dir();

        assert!(layer_dir.ends_with("vulkan-layer"));
        assert!(
            !layer_dir.starts_with(&installer.backup_root),
            "the layer must not live inside the backup store"
        );
    }

    #[test]
    fn two_games_share_one_layer_registration() {
        // The round trip that matters: undoing the first game must leave the
        // second working. Nothing here touches the real registry.
        let dir = tempfile::tempdir().expect("tempdir");
        let installer = Installer::with_registry(dir.path(), Box::new(FakeRegistry::default()));
        let one = Path::new("D:/Games/One");
        let two = Path::new("D:/Games/Two");

        assert!(matches!(
            installer.register_vulkan_layer(one, "ReShade64.json"),
            Ok(layer::Registered::Added { first: true, .. })
        ));
        assert!(matches!(
            installer.register_vulkan_layer(two, "ReShade64.json"),
            Ok(layer::Registered::AlreadyOurs { games: 2, .. })
        ));

        assert!(
            matches!(
                installer.deregister_vulkan_layer(one, "ReShade64.json"),
                Ok(layer::Deregistered::StillWanted { games: 1, .. })
            ),
            "the second game still wants it"
        );
        assert!(matches!(
            installer.deregister_vulkan_layer(two, "ReShade64.json"),
            Ok(layer::Deregistered::Removed { .. })
        ));
    }

    #[test]
    fn recovery_on_a_fresh_profile_finds_nothing_and_does_not_fail() {
        let dir = tempfile::tempdir().expect("tempdir");
        let installer = Installer::new(dir.path());
        assert!(installer.recover_at_startup().is_empty());
    }

    #[test]
    fn status_is_none_when_nothing_was_installed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let installer = Installer::new(dir.path());
        assert!(installer
            .status(&dir.path().join("game"))
            .expect("status")
            .is_none());
    }

    #[test]
    fn a_second_install_for_the_same_game_is_refused_rather_than_queued() {
        let dir = tempfile::tempdir().expect("tempdir");
        let installer = Installer::new(dir.path());
        let game = dir.path().join("game");

        let held = installer
            .locks
            .try_acquire(&key_of(&game))
            .expect("first lock");
        assert!(installer.is_busy(&game));

        // A plan is not needed: the lock is taken before any work.
        let plan = Plan {
            route: plan::Route::NativeDll,
            install_dir: String::new(),
            steps: Vec::new(),
            warnings: Vec::new(),
            write_bytes: 0,
            backup_bytes: 0,
            changes: 0,
        };
        let outcome = installer.apply(&game, &plan, dir.path(), false);
        assert_eq!(outcome.err().map(|error| error.code), Some(Code::JobBusy));

        drop(held);
        assert!(!installer.is_busy(&game));
    }
}
