//! What NeuralSwap would install in each game here, and why.
//!
//! `cargo run -p neuralswap-core --example show_recipes`
//!
//! The point of running this against a real library rather than fixtures: a
//! recipe is the list a user is asked to accept, so the only way to know it is
//! sensible is to read the ones the machine actually produces.

use std::sync::Mutex;

use neuralswap_core::components::catalog::default_catalog;
use neuralswap_core::install::recipe;
use neuralswap_core::jobs::Cancel;
use neuralswap_core::pe::PeCache;
use neuralswap_core::scan::capability::Feature;
use neuralswap_core::scan::integration::assess;
use neuralswap_core::scan::scan_folder;
use neuralswap_core::{library, platform};

fn main() {
    let catalog = default_catalog();
    let games = library::discover(&platform::roots());
    let cache = Mutex::new(PeCache::new_empty());
    let cancel = Cancel::new();
    let card = platform::gpu::best_nvidia().map(|found| found.generation);

    for game in &games {
        let scan = scan_folder(&game.dir, &cache, &cancel);
        let Some(candidate) = scan.candidates.first() else {
            continue;
        };

        let exe = game.dir.join(candidate.rel.replace('\\', "/"));
        let imports = neuralswap_core::pe::PeFile::with(&exe, |pe| pe.import_names(), Vec::new());
        let beside: Vec<String> = exe
            .parent()
            .and_then(|dir| std::fs::read_dir(dir).ok())
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_lowercase())
            .collect();
        let has_native_dlss = beside.iter().any(|name| name.starts_with("nvngx"));

        let found = assess(&imports, &beside, has_native_dlss);
        // The footprint survey is of the directory the runtime goes into,
        // which is where another tool's files would be too.
        let survey = neuralswap_core::scan::footprints::survey(exe.parent().unwrap_or(&game.dir));
        let game_feeds = Feature::fed_by_game(&scan.runtime_files);

        // The best route the assessment offers is the one a user would be
        // shown first.
        let Some(&route) = found.routes.first() else {
            continue;
        };
        let built = recipe::build(
            &catalog,
            route,
            &Feature::ALL,
            found.integration,
            &game_feeds,
            &survey,
            card,
        );

        println!("\n{} - {:?} via {:?}", game.name, found.integration, route);
        if built.steps.is_empty() {
            println!("  nothing to install");
        }
        for step in &built.steps {
            let verb = if step.already_present {
                "have   "
            } else {
                "install"
            };
            println!(
                "  {verb} {:<22} {:?} - {}",
                step.component, step.role, step.because
            );
        }
        for item in &built.delivers {
            println!("  gives   {:<22} {:?}", item.feature.label(), item.quality);
        }
        for item in &built.refuses {
            println!("  refuses {:<22} {}", item.feature.label(), item.reason);
        }
        for clash in &built.clashes {
            println!(
                "  CLASH   {:?} vs {} - {}",
                clash.tool, clash.with, clash.reason
            );
        }
        println!("  runnable: {}", built.is_runnable());
    }
}
