//! Finding the runtime files already on this machine.
//!
//! The runtimes cannot be shipped or mirrored - the RTX SDK licence permits
//! distribution only as part of an application that uses them, and the neural
//! rendering runtime has no public release at all. Every other tool in this
//! space resolves that by mirroring them anyway. The alternative, and the whole
//! point of this module, is that the files are *already here*: in the games
//! that shipped them and in the driver's own store. Finding them is a search
//! problem rather than a distribution problem.
//!
//! Two sources, and the difference between them matters:
//!
//! | Source | Confidence |
//! | --- | --- |
//! | The driver's store | Genuine NVIDIA build by definition - the driver installer put it there |
//! | An installed game | Genuine if the game shipped it; unknown if a tool replaced it |
//!
//! So a candidate carries where it came from, and a game copy that another
//! tool demonstrably installed is reported as such rather than offered as a
//! pristine source. Nothing is hashed during the search: the neural rendering
//! runtime is around 158 MB and there is no reason to read a hundred megabytes
//! per game to build a list. The digest is taken when one is chosen, which is
//! also when the planner needs it.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::scan::capability::Feature;
use crate::scan::folder::{Provenance, RuntimeFile};

/// Where a candidate was found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Origin {
    /// The driver's own file store. Put there by NVIDIA's installer, so it is
    /// a genuine build without needing to check anything.
    Driver,
    /// Beside a game's executable.
    Game {
        name: String,
        /// True when this looks like the set the game shipped, rather than
        /// something installed over it.
        as_shipped: bool,
    },
    /// A folder the user pointed at.
    Folder,
}

impl Origin {
    /// How much the provenance can be relied on, for ordering.
    ///
    /// Lower sorts first. The driver's copy wins because its provenance needs
    /// no inference; a game's shipped set is next; a file some tool installed
    /// is last, because we cannot say what it is.
    const fn confidence(&self) -> u8 {
        match self {
            Origin::Driver => 0,
            Origin::Game {
                as_shipped: true, ..
            } => 1,
            Origin::Folder => 2,
            Origin::Game {
                as_shipped: false, ..
            } => 3,
        }
    }

    pub fn label(&self) -> String {
        match self {
            Origin::Driver => "your graphics driver".to_owned(),
            Origin::Game {
                name,
                as_shipped: true,
            } => name.clone(),
            Origin::Game {
                name,
                as_shipped: false,
            } => format!("{name} (installed there by something, not shipped)"),
            Origin::Folder => "a folder you chose".to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    pub path: PathBuf,
    /// The file name, which is what an install would write.
    pub file_name: String,
    pub feature: Feature,
    pub version: Option<String>,
    pub size: u64,
    pub origin: Origin,
}

/// Where the driver keeps its own copies.
///
/// Verified on this machine: `nvngx.dll`, `_nvngx.dll` and a 9.3 MB
/// `nvngx_dlssg.dll` live under `nvhmi.inf_amd64_*`, alongside
/// `nvngx_update.exe` - which is the over-the-air machinery, on disk. The
/// driver does not carry the super resolution, ray reconstruction or neural
/// rendering runtimes, so this covers one feature of four.
fn driver_store_roots() -> Vec<PathBuf> {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_owned());
    let repository = Path::new(&root)
        .join("System32")
        .join("DriverStore")
        .join("FileRepository");

    let Ok(entries) = std::fs::read_dir(&repository) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter(|entry| {
            // The NVIDIA display driver packages. Named by their INF, and the
            // set of prefixes has changed across driver generations, so this
            // matches the vendor prefix rather than one exact package.
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with("nv"))
        })
        .map(|entry| entry.path())
        .collect()
}

/// Runtimes the installed driver provides.
pub fn from_driver_store() -> Vec<Candidate> {
    let mut found = Vec::new();
    for directory in driver_store_roots() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let Some(feature) = Feature::from_runtime(name) else {
                continue;
            };
            let Ok(meta) = entry.metadata() else { continue };
            if !meta.is_file() {
                continue;
            }
            found.push(Candidate {
                version: version_of(&path),
                file_name: name.to_owned(),
                feature,
                size: meta.len(),
                origin: Origin::Driver,
                path,
            });
        }
    }
    found
}

/// Runtimes a game has beside its executable.
///
/// Takes the scan's own runtime list rather than walking again, so provenance
/// comes along with it: a file whose version matches its neighbours looks like
/// part of the shipped set, and one that stands out was installed over it.
pub fn from_game(name: &str, game_dir: &Path, runtime_files: &[RuntimeFile]) -> Vec<Candidate> {
    use std::collections::BTreeMap;

    // A backup sibling left by another tool is stronger evidence than the
    // version cohort, and catches the case the cohort cannot: this machine has
    // a game whose whole runtime set is one version, with a tool-installed
    // neural rendering runtime among it. The cohort calls that consistent; the
    // `.original` beside it does not.
    //
    // Surveyed once per directory rather than per file, because a game with
    // eight runtimes beside one executable would otherwise list it eight
    // times.
    let mut surveyed: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();

    runtime_files
        .iter()
        .filter_map(|file| {
            let relative = file.rel.replace('\\', "/");
            let file_name = relative.rsplit('/').next().unwrap_or(&relative).to_owned();
            let feature = Feature::from_runtime(&file_name)?;
            let path = game_dir.join(&relative);
            let size = std::fs::metadata(&path).ok()?.len();

            let directory = path.parent().unwrap_or(game_dir).to_path_buf();
            let displaced = surveyed.entry(directory.clone()).or_insert_with(|| {
                crate::scan::footprints::survey(&directory)
                    .displaced
                    .into_iter()
                    .map(|entry| entry.file.to_lowercase())
                    .collect()
            });

            let cohort_agrees = matches!(
                file.provenance,
                // `OurInstall` is ours and fine to reuse; the two "differs"
                // verdicts mean somebody replaced it and we cannot say with
                // what.
                Provenance::ConsistentWithSiblings | Provenance::OurInstall
            );
            let somebody_installed_it = displaced.contains(&file_name.to_lowercase());

            Some(Candidate {
                version: file.version.clone(),
                origin: Origin::Game {
                    name: name.to_owned(),
                    as_shipped: cohort_agrees && !somebody_installed_it,
                },
                file_name,
                feature,
                size,
                path,
            })
        })
        .collect()
}

/// Merge and order candidates: best provenance first, then newest version.
///
/// Duplicates are collapsed on `(feature, version)` - the same runtime found
/// in four games is one choice, not four - keeping whichever copy has the
/// better provenance.
pub fn rank(mut candidates: Vec<Candidate>) -> Vec<Candidate> {
    candidates.sort_by(|left, right| {
        left.feature
            .cmp(&right.feature)
            // Newest first.
            .then_with(|| newest_first(&left.version, &right.version))
            .then_with(|| left.origin.confidence().cmp(&right.origin.confidence()))
            .then_with(|| left.path.cmp(&right.path))
    });
    candidates.dedup_by(|later, kept| {
        // The list is already ordered, so the one kept is the better of any
        // pair that describes the same thing.
        later.feature == kept.feature && later.version == kept.version
    });
    candidates
}

fn newest_first(left: &Option<String>, right: &Option<String>) -> std::cmp::Ordering {
    use crate::install::version::{compare_versions, parse_version};
    match (
        parse_version(left.as_deref()),
        parse_version(right.as_deref()),
    ) {
        (Some(a), Some(b)) => compare_versions(&b, &a),
        // A file with a readable version is more useful than one without, so
        // it sorts ahead.
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

/// The newest candidate for one feature, if any.
pub fn best_for(candidates: &[Candidate], feature: Feature) -> Option<&Candidate> {
    candidates.iter().find(|item| item.feature == feature)
}

fn version_of(path: &Path) -> Option<String> {
    crate::pe::PeFile::with(path, |pe| pe.file_version(), None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::folder::RuntimeKind;

    fn runtime(rel: &str, version: &str, provenance: Provenance) -> RuntimeFile {
        RuntimeFile {
            rel: rel.to_owned(),
            kind: RuntimeKind::Dlss,
            version: Some(version.to_owned()),
            provenance,
        }
    }

    fn candidate(feature: Feature, version: &str, origin: Origin) -> Candidate {
        Candidate {
            path: PathBuf::from(format!("x/{version}/{}", feature.runtime())),
            file_name: feature.runtime().to_owned(),
            feature,
            version: Some(version.to_owned()),
            size: 1,
            origin,
        }
    }

    fn game(name: &str, as_shipped: bool) -> Origin {
        Origin::Game {
            name: name.to_owned(),
            as_shipped,
        }
    }

    #[test]
    fn a_game_contributes_its_runtimes_with_provenance() {
        let dir = tempfile::tempdir().expect("tempdir");
        let game_dir = dir.path();
        std::fs::create_dir_all(game_dir.join("bin/x64")).expect("dirs");
        std::fs::write(game_dir.join("bin/x64/nvngx_dlss.dll"), b"shipped").expect("write");
        std::fs::write(game_dir.join("bin/x64/nvngx_dlssnr.dll"), b"installed").expect("write");

        let files = vec![
            runtime(
                "bin/x64/nvngx_dlss.dll",
                "310.1.0.0",
                Provenance::ConsistentWithSiblings,
            ),
            runtime(
                "bin/x64/nvngx_dlssnr.dll",
                "310.8.0.0",
                Provenance::VersionDiffersFromSiblings,
            ),
        ];
        let found = from_game("Some Game", game_dir, &files);
        assert_eq!(found.len(), 2);

        let shipped = found
            .iter()
            .find(|item| item.feature == Feature::SuperResolution)
            .expect("the upscaler");
        assert_eq!(shipped.origin, game("Some Game", true));

        // The odd one out was installed over the shipped set, and is offered
        // with that said rather than as a pristine source.
        let installed = found
            .iter()
            .find(|item| item.feature == Feature::NeuralRendering)
            .expect("the NR runtime");
        assert_eq!(installed.origin, game("Some Game", false));
        assert!(installed.origin.label().contains("not shipped"));
    }

    #[test]
    fn a_backup_sibling_overrides_a_cohort_that_looks_consistent() {
        // The case the version cohort cannot see, and it is on this machine: a
        // game whose entire runtime set is one version, with a tool-installed
        // runtime sitting among it at that same version. The cohort calls it
        // consistent. The `.original` beside it says otherwise, and it wins.
        let dir = tempfile::tempdir().expect("tempdir");
        let game_dir = dir.path();
        std::fs::create_dir_all(game_dir.join("bin")).expect("dirs");
        for name in [
            "nvngx_dlss.dll",
            "nvngx_dlssnr.dll",
            "nvngx_dlssnr.dll.original",
        ] {
            std::fs::write(game_dir.join("bin").join(name), b"x").expect("write");
        }

        let files = vec![
            runtime(
                "bin/nvngx_dlss.dll",
                "310.8.0.0",
                Provenance::ConsistentWithSiblings,
            ),
            runtime(
                "bin/nvngx_dlssnr.dll",
                "310.8.0.0",
                Provenance::ConsistentWithSiblings,
            ),
        ];
        let found = from_game("A Game", game_dir, &files);

        let shipped = found
            .iter()
            .find(|item| item.feature == Feature::SuperResolution)
            .expect("the upscaler");
        assert_eq!(shipped.origin, game("A Game", true));

        let installed = found
            .iter()
            .find(|item| item.feature == Feature::NeuralRendering)
            .expect("the NR runtime");
        assert_eq!(
            installed.origin,
            game("A Game", false),
            "a .original sibling means a tool put this here"
        );
    }

    #[test]
    fn the_driver_copy_is_preferred_over_a_games() {
        // Its provenance needs no inference: NVIDIA's installer put it there.
        let ranked = rank(vec![
            candidate(Feature::FrameGeneration, "310.8.0.0", game("A Game", true)),
            candidate(Feature::FrameGeneration, "310.8.0.0", Origin::Driver),
        ]);
        assert_eq!(ranked.len(), 1, "one version is one choice");
        assert_eq!(ranked[0].origin, Origin::Driver);
    }

    #[test]
    fn the_same_version_in_four_games_is_one_choice() {
        let ranked = rank(
            ["A", "B", "C", "D"]
                .iter()
                .map(|name| candidate(Feature::SuperResolution, "310.8.0.0", game(name, true)))
                .collect(),
        );
        assert_eq!(ranked.len(), 1);
    }

    #[test]
    fn newer_versions_come_first_and_different_versions_both_stay() {
        let ranked = rank(vec![
            candidate(Feature::SuperResolution, "310.1.0.0", game("Old", true)),
            candidate(Feature::SuperResolution, "310.9.0.0", game("New", true)),
            candidate(Feature::SuperResolution, "310.8.0.0", game("Mid", true)),
        ]);
        assert_eq!(
            ranked
                .iter()
                .map(|item| item.version.as_deref().unwrap_or(""))
                .collect::<Vec<_>>(),
            vec!["310.9.0.0", "310.8.0.0", "310.1.0.0"]
        );
    }

    #[test]
    fn a_file_installed_by_something_else_sorts_below_a_shipped_one() {
        // Same version from two games, one of them clearly modified. The
        // shipped copy is the one to reuse.
        let ranked = rank(vec![
            candidate(
                Feature::NeuralRendering,
                "310.8.0.0",
                game("Modified", false),
            ),
            candidate(Feature::NeuralRendering, "310.8.0.0", game("Shipped", true)),
        ]);
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].origin, game("Shipped", true));
    }

    #[test]
    fn a_version_that_could_not_be_read_sorts_last_but_is_kept() {
        // Still offered - plenty of DLLs carry no version resource - just not
        // ahead of one we can identify.
        let mut unknown = candidate(Feature::SuperResolution, "0", game("Mystery", true));
        unknown.version = None;
        let ranked = rank(vec![
            unknown,
            candidate(Feature::SuperResolution, "310.8.0.0", game("Known", true)),
        ]);
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].version.as_deref(), Some("310.8.0.0"));
        assert_eq!(ranked[1].version, None);
    }

    #[test]
    fn features_are_grouped_and_the_best_of_each_is_reachable() {
        let ranked = rank(vec![
            candidate(Feature::NeuralRendering, "310.8.0.0", Origin::Driver),
            candidate(Feature::SuperResolution, "310.1.0.0", game("A", true)),
            candidate(Feature::SuperResolution, "310.9.0.0", game("B", true)),
        ]);
        assert_eq!(
            best_for(&ranked, Feature::SuperResolution).and_then(|item| item.version.as_deref()),
            Some("310.9.0.0")
        );
        assert!(best_for(&ranked, Feature::NeuralRendering).is_some());
        assert!(best_for(&ranked, Feature::RayReconstruction).is_none());
    }

    #[test]
    fn searching_the_driver_store_never_panics_and_finds_what_is_there() {
        // Against the real machine. The driver ships the frame generation
        // runtime and the NGX loader, so on a machine with an NVIDIA driver
        // this finds something; on one without, it finds nothing and says so
        // quietly.
        let found = from_driver_store();
        for item in &found {
            assert!(item.path.is_file(), "{}", item.path.display());
            assert_eq!(item.origin, Origin::Driver);
            assert!(item.size > 0);
        }
        if found.is_empty() {
            eprintln!("SKIPPED the assertions: no NVIDIA driver store on this machine");
            return;
        }
        eprintln!(
            "driver store offers: {:?}",
            found
                .iter()
                .map(|item| (item.file_name.as_str(), item.version.as_deref()))
                .collect::<Vec<_>>()
        );
    }
}
