//! Scan a real folder from the command line.
//!
//! `cargo run -p neuralswap-core --example scan_dir -- "C:\Games\Something"`
//!
//! Exists so the scanner can be pointed at an actual install and checked
//! against what a person knows is in there - the fixtures prove the policy,
//! and this proves the policy survives contact with a real directory tree.
#![allow(clippy::expect_used, clippy::print_stdout)]

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

use neuralswap_core::jobs::Cancel;
use neuralswap_core::pe::PeCache;
use neuralswap_core::scan::scan_folder;

fn main() {
    let Some(target) = std::env::args().nth(1) else {
        eprintln!("usage: scan_dir <folder>");
        std::process::exit(2);
    };
    let dir = PathBuf::from(target);

    let cache = Mutex::new(PeCache::new_empty());
    let cancel = Cancel::new();

    let started = Instant::now();
    let scan = scan_folder(&dir, &cache, &cancel);
    let cold = started.elapsed();

    // The same folder again, to show what the cache is worth.
    let started = Instant::now();
    let _ = scan_folder(&dir, &cache, &cancel);
    let warm = started.elapsed();

    println!("folder: {}", scan.dir.display());
    println!("cold scan: {cold:?}   rescan: {warm:?}");
    {
        let cache = cache.lock().expect("cache lock");
        println!(
            "cache: {} entries, {} hits, {} misses",
            cache.len(),
            cache.stats().hits,
            cache.stats().misses
        );
    }
    println!(
        "walk: {} entries in {} dirs, {} ms   parse: {} binaries, {} ms",
        scan.stats.entries_examined,
        scan.stats.directories_walked,
        scan.stats.walk_ms,
        scan.stats.binaries_parsed,
        scan.stats.parse_ms
    );

    if let Some(reason) = scan.reason {
        println!("nothing installable: {reason:?}");
    }
    println!("\n{} candidate(s):", scan.candidates.len());
    for (index, candidate) in scan.candidates.iter().enumerate() {
        let api = candidate
            .api
            .as_ref()
            .map(|verdict| {
                format!(
                    "{}{}",
                    verdict.label,
                    if verdict.from_marker {
                        " (inferred)"
                    } else {
                        ""
                    }
                )
            })
            .unwrap_or_else(|| "unknown".to_owned());
        println!(
            "  {}{} {:<44} {:<22} {}-bit {:>9} KB{}",
            if Some(index) == scan.chosen { "*" } else { " " },
            if candidate.likely_helper { "~" } else { " " },
            candidate.rel,
            api,
            candidate.bitness,
            candidate.size / 1024,
            candidate
                .file_version
                .as_ref()
                .map(|v| format!("  v{v}"))
                .unwrap_or_default()
        );
    }
    if !scan.runtime_files.is_empty() {
        println!("\nruntime files already present:");
        for file in &scan.runtime_files {
            println!(
                "  {:<11} {:<14} {}{}",
                format!("{:?}", file.kind),
                format!("{:?}", file.provenance),
                file.rel,
                file.version
                    .as_ref()
                    .map(|v| format!("  v{v}"))
                    .unwrap_or_default()
            );
        }
    }
    if !scan.excluded.is_empty() {
        println!(
            "\nexcluded ({}): {}",
            scan.excluded.len(),
            scan.excluded.join(", ")
        );
    }
    println!("\n(* = recommended, ~ = probably a launcher)");
}
