//! Which install route a real game needs, and the evidence for it.
//!
//! `cargo run -p neuralswap-core --example show_routes`
//!
//! Route selection reads the import table, on the strength of NVIDIA's own
//! integration check - see `docs/how-dlss-works.md` §6. The only way to know
//! that works is to point it at games that really do integrate Streamline and
//! games that really do not.

use neuralswap_core::scan::integration::assess;
use neuralswap_core::scan::{scan_folder, RuntimeKind};
use neuralswap_core::{
    jobs::Cancel,
    library::{self},
    pe::{PeCache, PeFile},
    platform,
};
use std::sync::Mutex;

fn main() {
    let games = library::discover(&platform::roots());
    if games.is_empty() {
        println!("no games found");
        return;
    }

    let cache = Mutex::new(PeCache::new_empty());
    let cancel = Cancel::new();

    for game in games {
        let scan = scan_folder(&game.dir, &cache, &cancel);
        let Some(index) = scan.chosen else {
            println!("\n{}\n  no executable chosen", game.name);
            continue;
        };
        let Some(candidate) = scan.candidates.get(index) else {
            continue;
        };

        let exe = game.dir.join(candidate.rel.replace('\\', "/"));
        let imports = PeFile::with(&exe, |pe| pe.import_names(), Vec::new());

        // Runtime files beside the chosen executable are the second piece of
        // evidence: a DX11 game can ship DLSS without linking Streamline.
        let exe_dir = candidate
            .rel
            .rsplit_once(['/', '\\'])
            .map(|(dir, _)| dir.replace('\\', "/"))
            .unwrap_or_default();
        let has_native_dlss = scan.runtime_files.iter().any(|file| {
            file.kind == RuntimeKind::Dlss
                && file
                    .rel
                    .replace('\\', "/")
                    .rsplit_once('/')
                    .map(|(dir, _)| dir.to_owned())
                    .unwrap_or_default()
                    == exe_dir
        });

        let found = assess(&imports, has_native_dlss);
        println!("\n{}", game.name);
        println!("  executable : {}", candidate.rel);
        println!("  integration: {:?}", found.integration);
        println!(
            "  routes     : {}",
            found
                .routes
                .iter()
                .map(|route| format!("{route:?}"))
                .collect::<Vec<_>>()
                .join(" > ")
        );
        println!("  because    : {}", found.reason);

        // The imports the decision actually turned on.
        let interesting: Vec<&String> = imports
            .iter()
            .filter(|name| {
                name.starts_with("sl.")
                    || name.starts_with("nvngx")
                    || name.starts_with("d3d")
                    || name.starts_with("dxgi")
                    || name.starts_with("vulkan")
                    || name.starts_with("opengl")
            })
            .collect();
        println!("  imports    : {interesting:?}");
        if has_native_dlss {
            println!("  runtime    : DLSS files present beside the executable");
        }
    }
}
