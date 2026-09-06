//! What NeuralSwap would install in each game here, and why.
//!
//! `cargo run -p neuralswap-core --example show_recipes`
//!
//! The point of running this against a real library rather than fixtures: a
//! recipe is the list a user is asked to accept, so the only way to know it is
//! sensible is to read the ones the machine actually produces.

use std::sync::Mutex;

use neuralswap_core::components::catalog::default_catalog;
use neuralswap_core::install::{placement, recipe};
use neuralswap_core::jobs::Cancel;
use neuralswap_core::pe::PeCache;
use neuralswap_core::scan::capability::{Feature, Situation};
use neuralswap_core::scan::folder::{Provenance, RuntimeKind};
use neuralswap_core::scan::integration::assess;
use neuralswap_core::scan::scan_folder;
use neuralswap_core::{library, platform};

/// Display helper: an empty directory is the game root.
fn at(dir: &str) -> String {
    if dir.is_empty() {
        "<game root>".to_owned()
    } else {
        dir.to_owned()
    }
}

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
        // Provenance-filtered rather than "a file named nvngx is here", which
        // `assess` documents as the wrong claim: those files get left behind
        // and copied in by hand, so presence does not imply the game calls it.
        let has_native_dlss = scan.runtime_files.iter().any(|file| {
            file.kind == RuntimeKind::Dlss && file.provenance == Provenance::ConsistentWithSiblings
        });
        // Where the runtime loads from: the executable's own directory.
        let install_dir = candidate
            .rel
            .replace('\\', "/")
            .rsplit_once('/')
            .map(|(dir, _)| dir.to_owned())
            .unwrap_or_default();

        let found = assess(
            &imports,
            &beside,
            has_native_dlss,
            Some(candidate.bitness),
            candidate.api.as_ref().map(|verdict| verdict.api),
        );
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
            &Feature::ALL,
            &survey,
            &Situation {
                integration: found.integration,
                route,
                game_feeds: &game_feeds,
                card,
                direct3d: candidate.api.as_ref().and_then(|verdict| verdict.direct3d),
            },
        );

        println!("\n{} - {:?} via {:?}", game.name, found.integration, route);
        match &survey.proxy {
            Some(slot) => println!("  proxy slot: {} owned by {:?}", slot.file, slot.owner),
            None => println!("  proxy slot: free"),
        }
        for tool in survey.tools_present() {
            if survey.is_leftovers(tool) {
                println!("  leftovers:  {tool:?} is present but not loading");
            }
        }
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
        for item in placement::plan(
            &catalog,
            &built,
            &install_dir,
            &placement::Target {
                bitness: candidate.bitness,
                api: candidate.api.as_ref().map(|verdict| verdict.api),
                imports: &imports,
            },
        ) {
            match &item.delivery {
                placement::Delivery::Proxy { dir, as_name, from } => {
                    println!(
                        "  place   {:<22} {}/{as_name}  (renamed from {from})",
                        item.component,
                        at(dir)
                    )
                }
                placement::Delivery::VulkanLayer {
                    manifest,
                    layer,
                    library,
                } => println!(
                    "  MACHINE {:<22} Vulkan layer {layer} via {manifest} -> {library}                      (registry, affects every Vulkan app)",
                    item.component
                ),
                placement::Delivery::Copy { dir } => {
                    println!("  place   {:<22} {}/", item.component, at(dir))
                }
                placement::Delivery::ByHand { dir, from, files } => println!(
                    "  BY HAND {:<22} {}/  get from {from}: {}",
                    item.component,
                    at(dir),
                    files.join(", ")
                ),
            }
        }
        println!("  runnable: {}", built.is_runnable());
    }
}
