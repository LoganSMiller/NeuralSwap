// Tests may panic freely: a broken fixture should abort the run, loudly.
// The workspace keeps these lints strict for library code, where an
// unexplained panic reaches a user mid-install.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

//! Replays the behavioural vectors in `spec/` against the Rust core.
//!
//! These are the same JSON tables and the same byte-identical fixtures the
//! TypeScript reference is held to. That is the point: a reimplementation is
//! only trustworthy if it can be shown to make the same decisions, for the
//! same reasons, on the same inputs - including the awkward ones nobody would
//! think to retype.

use std::path::{Path, PathBuf};

use neuralswap_core::fsx::paths::{assert_safe_relative, is_inside, safe_path};

fn spec_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is src-tauri/crates/core
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../spec")
        .canonicalize()
        .expect("spec/ not found - run `npm run vectors`")
}

fn read_table(rel: &str) -> serde_json::Value {
    let file = spec_root().join(rel);
    let text = std::fs::read_to_string(&file)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", file.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("bad JSON in {rel}: {error}"))
}

/// The outcome of a guard, as the vectors record it: `ok` or the error code.
fn outcome<T>(result: neuralswap_core::Result<T>) -> String {
    match result {
        Ok(_) => "ok".to_owned(),
        Err(error) => error.code.as_str().to_owned(),
    }
}

#[test]
fn path_vectors_match() {
    let table = read_table("paths.json");
    let root = table["root"].as_str().expect("root");
    let root = Path::new(root);
    let cases = table["cases"].as_array().expect("cases");

    assert!(cases.len() >= 25, "the table lost cases: {}", cases.len());

    let mut checked = 0;
    for case in cases {
        let rel = case["rel"].as_str().expect("rel");
        let why = case["why"].as_str().unwrap_or("");
        let expect = case["expect"].as_str().expect("expect");

        let actual = outcome(assert_safe_relative(rel, root));
        assert_eq!(actual, expect, "case {rel:?} ({why})");
        checked += 1;
    }
    assert_eq!(checked, cases.len());
}

/// The vectors cannot express "and it must be refused for the right reason on
/// both platforms", so that part is asserted directly.
#[test]
fn path_rules_do_not_depend_on_the_host_platform() {
    let root = Path::new(if cfg!(windows) {
        "C:\\games\\example"
    } else {
        "/games/example"
    });

    // Windows-shaped hostile input must be refused when running on Linux too.
    // The TypeScript version leaned on Node's platform-specific `isAbsolute`,
    // so a UNC path was refused on Windows and accepted as an odd filename on
    // Linux. Deciding the rule ourselves makes both platforms agree.
    for rel in [
        "\\\\server\\share\\evil.dll",
        "C:\\Windows\\System32\\evil.dll",
        "bin\\..\\..\\escape.txt",
        "\\rooted\\path.dll",
    ] {
        assert_eq!(
            outcome(assert_safe_relative(rel, root)),
            "unsafePath",
            "{rel:?}"
        );
    }

    // And backslash segments must be split on every platform, or a `..` hides.
    assert_eq!(
        outcome(assert_safe_relative("bin\\x64\\game.exe", root)),
        "ok"
    );
}

#[test]
fn safe_path_refuses_a_symlinked_component() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let real = scratch.path().join("real");
    let outside = scratch.path().join("outside");
    std::fs::create_dir_all(&real).expect("mkdir real");
    std::fs::create_dir_all(&outside).expect("mkdir outside");

    // Creating a link needs Developer Mode or elevation on Windows. Where it
    // is unavailable the guarantee is untestable rather than untrue.
    let linked = {
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_dir(&outside, real.join("link")).is_ok()
        }
        #[cfg(not(windows))]
        {
            std::os::unix::fs::symlink(&outside, real.join("link")).is_ok()
        }
    };
    if !linked {
        eprintln!("skipping: symlink creation not permitted here");
        return;
    }

    assert_eq!(
        outcome(safe_path(&real, "link/payload.dll")),
        "symlinkRefused"
    );
    assert_eq!(outcome(safe_path(&real, "link")), "symlinkRefused");
    // A sibling that is not a link is still fine.
    assert_eq!(outcome(safe_path(&real, "plain/payload.dll")), "ok");
}

#[test]
fn is_inside_recognises_nesting_and_rejects_siblings() {
    let root = Path::new(if cfg!(windows) { "C:\\games" } else { "/games" });
    assert!(is_inside(&root.join("a").join("b"), &root.join("a")));
    assert!(is_inside(&root.join("a"), &root.join("a")));
    // `ab` shares a textual prefix with `a` but is not inside it - the bug a
    // naive `starts_with` on strings would introduce.
    assert!(!is_inside(&root.join("ab"), &root.join("a")));
    assert!(!is_inside(&root.join("a"), &root.join("a").join("b")));
}

// ---------------------------------------------------------------- archives

fn count_files(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(count_files(&path));
        } else {
            found.push(path);
        }
    }
    found
}

#[test]
fn archive_vectors_match_and_refuse_before_writing() {
    use neuralswap_core::zip::{extract_zip, Limits};

    let table = read_table("zip/cases.json");
    let cases = table["cases"].as_array().expect("cases");
    assert!(cases.len() >= 14, "the table lost cases: {}", cases.len());

    let scratch = tempfile::tempdir().expect("tempdir");

    for (index, case) in cases.iter().enumerate() {
        let file = case["file"].as_str().expect("file");
        let why = case["why"].as_str().unwrap_or("");
        let expect = case["expect"].as_str().expect("expect");

        let archive = spec_root().join(file);
        let destination = scratch.path().join(format!("case{index}"));

        let actual = outcome(extract_zip(&archive, &destination, Limits::default()));
        assert_eq!(actual, expect, "{file} ({why})");

        // The stronger half of the guarantee: a refusal leaves nothing behind.
        if actual != "ok" {
            let written = count_files(&destination);
            assert!(
                written.is_empty(),
                "{file} wrote {written:?} despite failing"
            );
        }
    }
}

#[test]
fn the_cve_archive_cannot_escape_its_destination() {
    use neuralswap_core::zip::{extract_zip, Limits};

    // GHSA-jmr9-qjv8-65gv in its exact shape: a symlink entry pointing out of
    // the tree, then a second entry writing through it. Asserted separately
    // from the table because the thing to prove is that the file it aimed at
    // does not appear anywhere - not merely that an error was returned.
    let scratch = tempfile::tempdir().expect("tempdir");
    let destination = scratch.path().join("dest");
    let sibling = scratch.path().join("escaped");
    std::fs::create_dir_all(&sibling).expect("mkdir sibling");

    let archive = spec_root().join("zip/symlink-escape.zip.bin");
    let error = extract_zip(&archive, &destination, Limits::default())
        .expect_err("the symlink archive must be refused");
    assert_eq!(error.code.as_str(), "zipEntryUnsafe");

    assert!(
        count_files(&destination).is_empty(),
        "wrote into the destination"
    );
    assert!(
        count_files(&sibling).is_empty(),
        "escaped into a sibling directory"
    );
    assert!(!sibling.join("payload.dll").exists());
}

#[test]
fn a_benign_archive_round_trips_with_verified_contents() {
    use neuralswap_core::zip::{extract_zip, Limits};

    let scratch = tempfile::tempdir().expect("tempdir");
    let destination = scratch.path().join("dest");
    let archive = spec_root().join("zip/benign.zip.bin");

    let result = extract_zip(&archive, &destination, Limits::default()).expect("benign archive");
    assert!(
        result.files.iter().any(|f| f == "readme.txt"),
        "{:?}",
        result.files
    );

    let readme = std::fs::read_to_string(destination.join("readme.txt")).expect("readme");
    assert_eq!(readme, "hello world");
    // Deflated and stored entries must both come out at their real length.
    assert_eq!(
        std::fs::metadata(destination.join("bin/tool.dll"))
            .expect("dll")
            .len(),
        4096
    );
    assert_eq!(
        std::fs::metadata(destination.join("bin/raw.bin"))
            .expect("bin")
            .len(),
        1024
    );
    assert!(destination.join("empty").is_dir());
}

#[test]
fn limits_refuse_an_archive_that_would_expand_too_far() {
    use neuralswap_core::zip::{extract_zip, Limits};

    let scratch = tempfile::tempdir().expect("tempdir");
    let archive = spec_root().join("zip/benign.zip.bin");

    let tight = Limits {
        max_entry_bytes: 1024,
        ..Limits::default()
    };
    assert_eq!(
        outcome(extract_zip(&archive, &scratch.path().join("a"), tight)),
        "zipTooLarge"
    );

    let total = Limits {
        max_total_bytes: 512,
        ..Limits::default()
    };
    assert_eq!(
        outcome(extract_zip(&archive, &scratch.path().join("b"), total)),
        "zipTooLarge"
    );

    let count = Limits {
        max_entries: 1,
        ..Limits::default()
    };
    assert_eq!(
        outcome(extract_zip(&archive, &scratch.path().join("c"), count)),
        "zipTooLarge"
    );
}

// ---------------------------------------------------------------- settings

#[test]
fn settings_vectors_match() {
    use neuralswap_core::settings::SettingsStore;

    let table = read_table("settings/cases.json");
    let cases = table["cases"].as_array().expect("cases");
    let scratch = tempfile::tempdir().expect("tempdir");

    for (index, case) in cases.iter().enumerate() {
        let file = case["file"].as_str().expect("file");
        let why = case["why"].as_str().unwrap_or("");
        let expect = &case["expect"];
        let expected_status = expect["status"].as_str().expect("status");

        // Opening a corrupt file quarantines it by renaming, so work on a copy
        // and leave the vector intact for the next implementation to read.
        let dir = scratch.path().join(format!("case{index}"));
        std::fs::create_dir_all(&dir).expect("mkdir case");
        let copy = dir.join("settings.json");
        let original = std::fs::read(spec_root().join(file)).expect("read vector");
        std::fs::write(&copy, &original).expect("write copy");

        if expected_status == "refused" {
            let expected_code = expect["code"].as_str().expect("code");
            let error = SettingsStore::open(&copy)
                .err()
                .unwrap_or_else(|| panic!("{file} ({why}) was accepted but must be refused"));
            assert_eq!(error.code.as_str(), expected_code, "{file} ({why})");
            // Refusing must not touch the file: it belongs to the newer build.
            assert_eq!(
                std::fs::read(&copy).expect("reread"),
                original,
                "{file} was modified"
            );
            continue;
        }

        let store = SettingsStore::open(&copy)
            .unwrap_or_else(|error| panic!("{file} ({why}) failed to open: {error}"));
        let health = store.health();
        let actual = serde_json::to_value(health.status).expect("status json");
        assert_eq!(
            actual.as_str().unwrap_or_default(),
            expected_status,
            "{file} ({why})"
        );

        // Compare the loaded settings field-for-field against the reference.
        let observed = serde_json::to_value(store.get()).expect("settings json");
        let reference = &expect["settings"];
        for key in [
            "schema",
            "theme",
            "lang",
            "groupGamesByStore",
            "autoScanDrives",
        ] {
            assert_eq!(&observed[key], &reference[key], "{file}: {key}");
        }
        for key in [
            "folders",
            "excludedRoots",
            "manual",
            "hidden",
            "posters",
            "scans",
            "addons",
        ] {
            assert_eq!(&observed[key], &reference[key], "{file}: {key}");
        }

        if expected_status == "quarantined" {
            // Set aside, never destroyed.
            let quarantine = health.quarantined_to.expect("nothing was quarantined");
            assert_eq!(
                std::fs::read(&quarantine).expect("read quarantine"),
                original,
                "{file}"
            );
        }
    }
}

#[test]
fn concurrent_updates_all_survive() {
    use neuralswap_core::settings::SettingsStore;
    use std::sync::Arc;

    let scratch = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(SettingsStore::open(scratch.path().join("settings.json")).expect("open"));

    // The failure this store exists to remove: many callers each doing
    // read-modify-write on the same document. Every one of the fifty must be
    // present afterwards, and reopening must agree.
    let mut threads = Vec::new();
    for index in 0..50 {
        let store = Arc::clone(&store);
        threads.push(std::thread::spawn(move || {
            store
                .update(|settings| {
                    settings.folders.push(format!(r"D:\Games\game{index}"));
                })
                .expect("update");
        }));
    }
    for thread in threads {
        thread.join().expect("join");
    }

    assert_eq!(store.get().folders.len(), 50);
    let reopened = SettingsStore::open(scratch.path().join("settings.json")).expect("reopen");
    assert_eq!(reopened.get().folders.len(), 50);
}

#[test]
fn an_unwritable_file_surfaces_as_an_error() {
    use neuralswap_core::settings::SettingsStore;

    // A directory where the settings file should be makes every write fail,
    // standing in for a read-only profile or a full disk.
    let scratch = tempfile::tempdir().expect("tempdir");
    let blocked = scratch.path().join("settings.json");
    let store = SettingsStore::open(&blocked).expect("open");
    std::fs::create_dir_all(&blocked).expect("mkdir blocker");

    let error = store
        .update(|settings| settings.lang = "sv".to_owned())
        .expect_err("a blocked write must be reported");
    assert_eq!(error.code.as_str(), "stateUnwritable");
    // Never silently swallowed: the UI can tell the user their settings are
    // not being saved, which the upstream `catch {}` made impossible.
    let health = store.health();
    assert_eq!(
        health.write_error.map(|e| e.code).as_deref(),
        Some("stateUnwritable")
    );
}

#[test]
fn a_previous_good_copy_is_kept_as_the_backup() {
    use neuralswap_core::settings::SettingsStore;

    let scratch = tempfile::tempdir().expect("tempdir");
    let file = scratch.path().join("settings.json");
    let store = SettingsStore::open(&file).expect("open");
    store.update(|s| s.lang = "cs".to_owned()).expect("first");
    store.update(|s| s.lang = "hu".to_owned()).expect("second");

    let backup = std::fs::read_to_string(format!("{}.bak", file.display())).expect("backup");
    assert!(
        backup.contains("\"cs\""),
        "backup should hold the previous value: {backup}"
    );
    assert_eq!(store.get().lang, "hu");
}

// --------------------------------------------------------------------- pe

/// The marker set the vectors were generated with, read from the table so the
/// two cannot drift.
fn pe_markers(table: &serde_json::Value) -> Vec<String> {
    table["markers"]
        .as_array()
        .expect("markers")
        .iter()
        .filter_map(|m| m.as_str().map(str::to_owned))
        .collect()
}

#[test]
fn pe_vectors_match() {
    use neuralswap_core::pe::PeFile;

    let table = read_table("pe/cases.json");
    let markers = pe_markers(&table);
    let marker_refs: Vec<&str> = markers.iter().map(String::as_str).collect();
    let cases = table["cases"].as_array().expect("cases");
    assert!(cases.len() >= 6, "the table lost cases: {}", cases.len());

    for case in cases {
        let file = case["file"].as_str().expect("file");
        let why = case["why"].as_str().unwrap_or("");
        let expect = &case["expect"];
        let path = spec_root().join(file);

        let observed = PeFile::with(
            &path,
            |pe| {
                Some((
                    pe.bitness(),
                    pe.machine(),
                    {
                        let mut names = pe.import_names();
                        names.sort();
                        names
                    },
                    pe.find_markers(&marker_refs),
                    pe.bytes_read(),
                ))
            },
            None,
        );

        if !expect["parses"].as_bool().unwrap_or(false) {
            assert!(observed.is_none(), "{file} should not parse ({why})");
            continue;
        }

        let (bitness, machine, imports, found, bytes_read) =
            observed.unwrap_or_else(|| panic!("{file} failed to parse ({why})"));

        assert_eq!(
            i64::from(bitness),
            expect["bitness"].as_i64().unwrap_or(0),
            "{file} bitness ({why})"
        );
        assert_eq!(
            i64::from(machine),
            expect["machine"].as_i64().unwrap_or(0),
            "{file} machine ({why})"
        );

        let expected_imports: Vec<String> = expect["imports"]
            .as_array()
            .expect("imports")
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect();
        assert_eq!(imports, expected_imports, "{file} imports ({why})");

        let expected_markers: Vec<String> = expect["markers"]
            .as_array()
            .expect("markers")
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect();
        assert_eq!(found, expected_markers, "{file} markers ({why})");

        // The overlay case is the one where IO volume is itself the guarantee:
        // appended data cannot hold a marker the loader will resolve, so
        // reading it is pure cost - and on a large library it dominates.
        if file.contains("overlay") {
            let size = case["sizeBytes"].as_u64().unwrap_or(u64::MAX);
            assert!(
                bytes_read < size / 4,
                "{file}: read {bytes_read} of {size} bytes - the overlay is being scanned"
            );
        }
    }
}

#[test]
fn a_real_system_binary_parses() {
    use neuralswap_core::pe::PeFile;

    // The synthetic fixtures prove the parser follows the specification as
    // written; a real Microsoft binary proves it as shipped.
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_owned());
    let path = std::path::PathBuf::from(root)
        .join("System32")
        .join("kernel32.dll");
    if !path.exists() {
        eprintln!("skipping: no system binary available");
        return;
    }

    let facts = PeFile::with(
        &path,
        |pe| {
            Some((
                pe.bitness(),
                pe.import_names(),
                pe.file_version(),
                pe.version_mentions("Microsoft"),
            ))
        },
        None,
    );
    let (bitness, imports, version, mentions) = facts.expect("kernel32.dll should parse");

    assert_eq!(bitness, 64);
    assert!(!imports.is_empty(), "kernel32 imports nothing?");
    assert!(
        imports.iter().any(|name| name.ends_with(".dll")),
        "expected DLL names, got {:?}",
        imports.iter().take(5).collect::<Vec<_>>()
    );
    let version = version.expect("kernel32 has a version resource");
    assert!(
        version.split('.').count() == 4 && version.split('.').all(|p| p.parse::<u32>().is_ok()),
        "unexpected version shape: {version}"
    );
    assert!(
        mentions,
        "kernel32's version resource should mention Microsoft"
    );
}

#[test]
fn the_pe_cache_answers_an_unchanged_file_without_re_reading() {
    use neuralswap_core::pe::{PeCache, Request};

    let markers = ["D3D12CreateDevice"];
    let request = Request {
        markers: &markers,
        rules: 1,
        ..Request::default()
    };
    let path = spec_root().join("pe/x64-d3d12.pe.bin");

    let mut cache = PeCache::new_empty();
    let first = cache.summarize(&path, &request);
    let second = cache.summarize(&path, &request);
    assert_eq!(first, second);
    assert_eq!(cache.stats().misses, 1);
    assert_eq!(cache.stats().hits, 1);

    // Changing the question must not be answered with the old question's
    // result, or a new detection generation silently inherits stale verdicts.
    let newer = Request {
        markers: &markers,
        rules: 2,
        ..Request::default()
    };
    cache.summarize(&path, &newer);
    assert_eq!(cache.stats().misses, 2);
    assert_eq!(cache.stats().evictions, 1);
}

#[test]
fn the_pe_cache_remembers_that_a_file_is_not_a_pe() {
    use neuralswap_core::pe::{PeCache, Request};

    let request = Request {
        rules: 1,
        ..Request::default()
    };
    let path = spec_root().join("zip/benign.zip.bin");

    let mut cache = PeCache::new_empty();
    assert!(cache.summarize(&path, &request).is_none());
    assert!(cache.summarize(&path, &request).is_none());
    // Remembering the negative is what stops a folder of data files being
    // re-examined on every single scan.
    assert_eq!(cache.stats().misses, 1);
    assert_eq!(cache.stats().hits, 1);
}

#[test]
fn the_pe_cache_survives_a_round_trip_through_json() {
    use neuralswap_core::pe::{PeCache, Request};

    let markers = ["D3D12CreateDevice"];
    let request = Request {
        markers: &markers,
        rules: 1,
        ..Request::default()
    };
    let path = spec_root().join("pe/x64-d3d12.pe.bin");

    let mut cache = PeCache::new_empty();
    cache.summarize(&path, &request);
    let text = serde_json::to_string(&cache).expect("serialise");

    let mut revived: PeCache = serde_json::from_str(&text).expect("deserialise");
    let summary = revived.summarize(&path, &request);
    assert!(summary.is_some());
    // Restored from disk on the next launch, so the first scan is already warm.
    assert_eq!(revived.stats().hits, 1);
    assert_eq!(revived.stats().misses, 0);
}

#[test]
fn version_ordering_vectors_match() {
    use neuralswap_core::install::relate;

    let table = read_table("install/versions.json");
    let cases = table["cases"].as_array().expect("cases");
    assert!(cases.len() >= 12, "the table lost cases: {}", cases.len());

    for case in cases {
        let why = case["why"].as_str().unwrap_or("");
        let package = case["packageVersion"].as_str();
        let present = case["presentVersion"].as_str();
        let expect = case["expect"].as_str().expect("expect");

        let actual = serde_json::to_value(relate(package, present)).expect("serialise");
        assert_eq!(
            actual.as_str(),
            Some(expect),
            "relate({package:?}, {present:?}) ({why})"
        );
    }
}

#[test]
fn install_plan_vectors_match() {
    use neuralswap_core::install::{build_plan, PlanInput};

    let table = read_table("install/plan.json");
    let cases = table["cases"].as_array().expect("cases");
    assert!(cases.len() >= 20, "the table lost cases: {}", cases.len());

    let mut refusals = 0;
    for case in cases {
        let name = case["name"].as_str().expect("name");
        let why = case["why"].as_str().unwrap_or("");
        let expect = &case["expect"];

        // The input travels as JSON, so the two implementations are handed
        // structurally identical data rather than each building its own.
        let request: PlanInput =
            serde_json::from_value(case["input"].clone()).unwrap_or_else(|error| {
                panic!("case {name}: input does not deserialise: {error}");
            });

        match build_plan(&request) {
            Ok(plan) => {
                assert!(
                    expect.get("refused").is_none(),
                    "case {name} ({why}): expected refusal {:?}, got a plan",
                    expect["refused"]
                );
                let actual = serde_json::to_value(&plan).expect("serialise");
                // Compared whole. A reason, a byte total or a warning that
                // differs is a divergence even when the file list agrees, and
                // those are exactly the parts a user reads.
                assert_eq!(&actual, expect, "case {name} ({why})");
            }
            Err(error) => {
                let expected = expect["refused"].as_str().unwrap_or_else(|| {
                    panic!(
                        "case {name} ({why}): refused with {} but a plan was expected",
                        error.code
                    );
                });
                assert_eq!(error.code.as_str(), expected, "case {name} ({why})");
                refusals += 1;
            }
        }
    }
    // The hostile half of the table is the half worth having, so a run that
    // somehow produced no refusals at all has stopped testing anything.
    assert!(refusals >= 8, "only {refusals} refusals replayed");
}

#[test]
fn journal_recovery_vectors_match() {
    use neuralswap_core::install::{decide_recovery, JournalState};

    let table = read_table("install/recovery.json");
    let cases = table["cases"].as_array().expect("cases");
    assert!(cases.len() >= 8, "the table lost cases: {}", cases.len());

    for case in cases {
        let name = case["name"].as_str().expect("name");
        let why = case["why"].as_str().unwrap_or("");
        let state: JournalState =
            serde_json::from_value(case["state"].clone()).unwrap_or_else(|error| {
                panic!("case {name}: state does not deserialise: {error}");
            });

        let actual = serde_json::to_value(decide_recovery(&state)).expect("serialise");
        assert_eq!(&actual, &case["expect"], "case {name} ({why})");
    }
}
