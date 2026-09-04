//! List the games this machine has installed.
//!
//! `cargo run -p neuralswap-core --example list_library`
//!
//! Reads storefront records only - no folder is scanned and nothing is written.
#![allow(clippy::expect_used, clippy::print_stdout)]

use std::time::Instant;

use neuralswap_core::library;
use neuralswap_core::platform;

fn main() {
    let started = Instant::now();
    let roots = platform::roots();
    let games = library::discover(&roots);
    let elapsed = started.elapsed();

    println!("searched:");
    println!(
        "  steam           : {}",
        roots
            .steam
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<not installed>".to_owned())
    );
    println!(
        "  epic manifests  : {}",
        roots
            .epic_manifests
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<not installed>".to_owned())
    );
    println!(
        "  xbox            : {}",
        if roots.xbox.is_empty() {
            "<none>".to_owned()
        } else {
            roots
                .xbox
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        }
    );

    println!("\n{} game(s) in {elapsed:?}:", games.len());
    for game in &games {
        println!(
            "  {:<10} {:<44} {}",
            game.source.label(),
            game.name,
            game.dir.display()
        );
    }
}
