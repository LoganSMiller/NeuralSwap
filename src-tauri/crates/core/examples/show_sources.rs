//! Every DLSS runtime already on this machine, and where it came from.
//!
//! `cargo run -p neuralswap-core --example show_sources`
//!
//! This is the answer to "where do the runtimes come from" that does not
//! involve redistributing them: they are already here. The list is what the
//! install screen offers, so the only way to know it is useful is to point it
//! at a real machine and see whether it finds anything worth having.

use neuralswap_core::install::discover::{self, Origin};
use neuralswap_core::jobs::Cancel;
use neuralswap_core::pe::PeCache;
use neuralswap_core::scan::capability::Feature;
use neuralswap_core::scan::scan_folder;
use neuralswap_core::{library, platform};
use std::sync::Mutex;

fn main() {
    let mut all = discover::from_driver_store();
    let from_driver = all.len();

    let games = library::discover(&platform::roots());
    let cache = Mutex::new(PeCache::new_empty());
    let cancel = Cancel::new();
    for game in &games {
        let scan = scan_folder(&game.dir, &cache, &cancel);
        all.extend(discover::from_game(
            &game.name,
            &game.dir,
            &scan.runtime_files,
        ));
    }
    let from_games = all.len() - from_driver;

    println!(
        "searched the driver store and {} game(s): {from_driver} + {from_games} runtime file(s)",
        games.len()
    );

    let ranked = discover::rank(all);
    println!("{} distinct runtime(s) available:\n", ranked.len());

    for feature in Feature::ALL {
        let mine: Vec<_> = ranked
            .iter()
            .filter(|item| item.feature == feature)
            .collect();
        println!("{} ({})", feature.label(), feature.runtime());
        if mine.is_empty() {
            println!("  none found on this machine");
        }
        for item in mine {
            let flag = match &item.origin {
                Origin::Driver => "driver",
                Origin::Game {
                    as_shipped: true, ..
                } => "shipped",
                Origin::Game {
                    as_shipped: false, ..
                } => "MODIFIED",
                Origin::Folder => "chosen",
            };
            println!(
                "  {:<12} {:<9} {:>7.1} MB  {}",
                item.version.as_deref().unwrap_or("unknown"),
                flag,
                item.size as f64 / 1_048_576.0,
                item.origin.label()
            );
        }
        println!();
    }
}
