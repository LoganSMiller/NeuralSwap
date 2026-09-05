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
/// `nvngx_update.exe` - which is the over-the-air machinery, on disk.
///
/// This location carries frame generation and nothing else, so on its own it
/// covers one feature of four. That was once written here as a fact about the
/// driver; it is only a fact about this directory. See [`from_ngx_store`],
/// which finds three of four somewhere else entirely.
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

/// The driver's NGX model store, which is a different thing from the driver
/// store and carries far more.
///
/// `%ProgramData%\NVIDIA\NGX\models` is where the driver keeps the runtimes it
/// serves for its own DLSS Override feature, and it is entirely
/// self-describing. Layout, verified on this machine:
///
/// ```text
/// models/
///   nvngx_config.txt                  which version is active, per component
///   dlss/versions/20318081/files/160_E658700.bin        74 MB
///   dlssd/versions/20318081/files/160_E658700.bin       80 MB
///   dlssg/versions/20318081/files/160_E658700.bin        7 MB
///   dlss_override/versions/20318081/files/160_E658700/
///       nvngx_package_config.txt      declares the three above
///       sl.dlss.dll, sl.dlss_d.dll, ...
/// ```
///
/// A `nvngx_package_config.txt` holds one comma-separated row per file:
///
/// ```text
/// dlss, 310.7.129, .bin, nvngx_dlss.dll
/// sl_common_0, 2.14.0, .dll, sl.common.dll
/// ```
///
/// That is `component, version, stored extension, real name` - so the store
/// declares both what a file *is* and what it should be called when installed,
/// which is exactly what this module otherwise has to infer. The file itself
/// sits either in the package directory or at
/// `<component>/versions/<key>/files/<prefix>_<app><ext>`, sharing the version
/// key across components so a matched set stays matched.
///
/// # Two things this corrects
///
/// The comment on [`driver_store_roots`] used to say the driver carries only
/// frame generation, "one feature of four". That was true of the driver store
/// and false of the machine: the NGX store has super resolution, ray
/// reconstruction *and* frame generation at 310.7.129. Three of four, from a
/// genuine NVIDIA source, with no redistribution.
///
/// The fourth is still missing, and its absence is informative: there is no
/// `sl.dlss_nr.dll` and no neural rendering runtime anywhere in the store. The
/// driver does not carry neural rendering, so it cannot be sourced this way.
///
/// # Why the declared version is trusted over the file's own
///
/// These files are hundreds of megabytes of model weights around a thin PE
/// wrapper, and scanning one for a `VS_FIXEDFILEINFO` signature finds a match
/// inside the weights long before the real resource - which reads out as
/// `46863.0.46863.4696`. The manifest says `310.7.129`. Where the two differ
/// the manifest wins, because it is a statement by the installer rather than a
/// guess about a byte pattern.
pub fn from_ngx_store() -> Vec<Candidate> {
    let Some(root) = ngx_store_root() else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for package in package_configs(&root) {
        let Some(files) = package.parent() else {
            continue;
        };
        // `<component>/versions/<key>/files/<prefix>_<app>` - the key is three
        // levels up from the package directory, and is what ties a component's
        // file to the package that declares it.
        let Some(version_key) = files
            .parent()
            .and_then(|dir| dir.parent())
            .and_then(|dir| dir.file_name())
            .map(|key| key.to_owned())
        else {
            continue;
        };
        let stem = files.file_name().map(|name| name.to_owned());

        let Ok(text) = std::fs::read_to_string(&package) else {
            continue;
        };
        for row in text.lines() {
            let Some(row) = PackageRow::parse(row) else {
                continue;
            };
            let Some(feature) = Feature::from_runtime(&row.real_name) else {
                // Streamline plugins and the components with no feature of
                // their own - sl.common, sl.nis, sl.pcl - are declared here
                // too. They matter for an install recipe, not for a runtime
                // candidate, so they are skipped rather than guessed at.
                continue;
            };

            // In the package directory under its real name, or at the
            // component path under the stored name. Both layouts occur.
            let direct = files.join(&row.real_name);
            let indirect = stem.as_ref().map(|stem| {
                let mut name = stem.clone();
                name.push(&row.stored_ext);
                root.join(&row.component)
                    .join("versions")
                    .join(&version_key)
                    .join("files")
                    .join(name)
            });
            let path = [Some(direct), indirect]
                .into_iter()
                .flatten()
                .find(|path| path.is_file());
            let Some(path) = path else { continue };
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };

            found.push(Candidate {
                // Declared, not read. See the note above.
                version: Some(row.version),
                file_name: row.real_name,
                feature,
                size: meta.len(),
                origin: Origin::Driver,
                path,
            });
        }
    }
    found.sort_by(|left, right| left.path.cmp(&right.path));
    found.dedup_by(|left, right| left.path == right.path);
    found
}

fn ngx_store_root() -> Option<PathBuf> {
    let base = std::env::var("ProgramData").ok()?;
    let root = Path::new(&base).join("NVIDIA").join("NGX").join("models");
    root.is_dir().then_some(root)
}

/// Every `nvngx_package_config.txt` under the store.
///
/// The nesting is fixed at four levels
/// (`<component>/versions/<key>/files/<prefix>_<app>`), so this walks to that
/// depth rather than recursing the whole tree - which would otherwise mean
/// stepping through several gigabytes of model directories to find text files.
fn package_configs(root: &Path) -> Vec<PathBuf> {
    const CONFIG: &str = "nvngx_package_config.txt";
    let children = |dir: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .map(|entries| {
                entries
                    .flatten()
                    .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
                    .map(|entry| entry.path())
                    .collect()
            })
            .unwrap_or_default()
    };

    let mut found = Vec::new();
    for component in children(root) {
        for key in children(&component.join("versions")) {
            for package in children(&key.join("files")) {
                let config = package.join(CONFIG);
                if config.is_file() {
                    found.push(config);
                }
            }
        }
    }
    found
}

/// One row of a package config.
struct PackageRow {
    component: String,
    version: String,
    stored_ext: String,
    real_name: String,
}

impl PackageRow {
    fn parse(row: &str) -> Option<PackageRow> {
        let mut fields = row.split(',').map(str::trim);
        let component = fields.next()?;
        let version = fields.next()?;
        let stored_ext = fields.next()?;
        let real_name = fields.next()?;
        // Four fields exactly. A row with more is a format this does not
        // understand, and reading the first four of it would be a guess.
        if fields.next().is_some() || component.is_empty() || real_name.is_empty() {
            return None;
        }
        // The name is used to build a path, so anything that could escape the
        // store is refused rather than sanitised.
        if real_name.contains(['/', '\\', ':']) || real_name.starts_with('.') {
            return None;
        }
        Some(PackageRow {
            component: component.to_owned(),
            version: version.to_owned(),
            stored_ext: stored_ext.to_owned(),
            real_name: real_name.to_owned(),
        })
    }
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

    #[test]
    fn a_package_row_needs_exactly_four_fields() {
        let row = PackageRow::parse("dlss, 310.7.129, .bin, nvngx_dlss.dll").expect("four fields");
        assert_eq!(row.component, "dlss");
        assert_eq!(row.version, "310.7.129");
        assert_eq!(row.stored_ext, ".bin");
        assert_eq!(row.real_name, "nvngx_dlss.dll");

        // The Streamline rows use the same shape.
        let plugin = PackageRow::parse("sl_common_0, 2.14.0, .dll, sl.common.dll").expect("plugin");
        assert_eq!(plugin.real_name, "sl.common.dll");

        // Anything else is a format this does not understand. Reading the
        // first four fields of a five-field row would be a guess about a
        // format change, and the cost of guessing is a wrong install.
        assert!(PackageRow::parse("dlss, 310.7.129, .bin").is_none());
        assert!(PackageRow::parse("dlss, 310.7.129, .bin, a.dll, extra").is_none());
        assert!(PackageRow::parse("").is_none());
        assert!(PackageRow::parse("# a comment").is_none());
    }

    #[test]
    fn a_package_row_cannot_name_a_path_outside_the_store() {
        // The declared name is joined onto a directory, so a row that walks
        // out of the store has to be refused rather than cleaned up: the file
        // it points at would be installed into a game folder under a name we
        // chose to trust.
        for hostile in [
            "dlss, 1, .bin, ..\\..\\..\\Windows\\System32\\evil.dll",
            "dlss, 1, .bin, ../../evil.dll",
            "dlss, 1, .bin, C:\\Windows\\System32\\evil.dll",
            "dlss, 1, .bin, .hidden",
        ] {
            assert!(PackageRow::parse(hostile).is_none(), "{hostile}");
        }
    }

    #[test]
    fn the_ngx_store_is_absent_without_error() {
        // Every discovery source has to be safe to call on a machine that
        // does not have it - an AMD box, or a fresh install. Reporting
        // nothing is the answer; failing is not.
        let found = from_ngx_store();
        for candidate in &found {
            assert_eq!(candidate.origin, Origin::Driver);
            assert!(candidate.size > 0, "{candidate:?}");
            assert!(candidate.version.is_some(), "{candidate:?}");
        }
    }

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
