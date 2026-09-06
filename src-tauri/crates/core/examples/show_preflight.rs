//! Run the install checks against the real games on this machine.
//!
//! Writes nothing: a preflight only reads, plus one probe file it removes
//! again. The package is a temporary directory holding a stand-in runtime, so
//! the checks have something to plan against.

use std::sync::Mutex;

use neuralswap_core::install::{plan, preflight};
use neuralswap_core::jobs::Cancel;
use neuralswap_core::library;
use neuralswap_core::pe::PeCache;
use neuralswap_core::platform;
use neuralswap_core::scan::folder::RuntimeKind;
use neuralswap_core::scan::scan_folder;

fn main() {
    let (Ok(package), Ok(backups)) = (tempfile::tempdir(), tempfile::tempdir()) else {
        println!("could not make a temporary directory");
        return;
    };
    // A stand-in for a real runtime. Only its name matters to the checks that
    // decide which features an install would write.
    let contents = b"not a real runtime";
    if std::fs::write(package.path().join("nvngx_dlss.dll"), contents).is_err() {
        println!("could not write the stand-in runtime");
        return;
    }

    let games = library::discover(&platform::roots());
    let cache = Mutex::new(PeCache::new_empty());
    let cancel = Cancel::new();

    for game in &games {
        let scan = scan_folder(&game.dir, &cache, &cancel);
        // Install beside the executable the scan ranked first, which is where
        // NGX would look for the runtime.
        let Some(candidate) = scan.candidates.first() else {
            continue;
        };
        let install_dir = candidate
            .rel
            .replace('\\', "/")
            .rsplit_once('/')
            .map(|(dir, _)| dir.to_owned())
            .unwrap_or_default();

        let built = plan::build_plan(&plan::PlanInput {
            route: plan::Route::NativeDll,
            install_dir: install_dir.clone(),
            present: vec![],
            pkg: vec![plan::PackageFile {
                name: "nvngx_dlss.dll".to_owned(),
                kind: RuntimeKind::Dlss,
                version: Some("310.8.0.0".to_owned()),
                size: contents.len() as u64,
                sha256: neuralswap_core::hash::hash_bytes(contents),
            }],
        });
        let Ok(built) = built else { continue };

        let report = preflight::preflight(&preflight::Request {
            game_dir: &game.dir,
            plan: &built,
            source_dir: package.path(),
            backup_dir: backups.path(),
            requires: None,
            anti_cheat_acknowledged: false,
        });

        println!("\n{}", game.name);
        println!(
            "  into: {}",
            if install_dir.is_empty() {
                "<game root>"
            } else {
                &install_dir
            }
        );
        for check in &report.checks {
            // The one this example exists for is printed in full; the rest are
            // summarised, so the interesting line does not scroll away.
            let mark = match check.outcome {
                preflight::CheckOutcome::Pass => "ok  ",
                preflight::CheckOutcome::Warn => "warn",
                preflight::CheckOutcome::Fail => "FAIL",
                preflight::CheckOutcome::Unknown => "?   ",
            };
            let interesting = check.name == preflight::CheckName::DriverOverride
                || check.outcome != preflight::CheckOutcome::Pass;
            if interesting {
                println!("  {mark} {:?}: {}", check.name, check.detail);
            }
        }
        println!("  installable: {}", report.ok);
    }
}
