//! Inspect a DLL that is supposed to be ReShade.

fn main() {
    for path in std::env::args().skip(1) {
        let found =
            neuralswap_core::scan::footprints::inspect_injector(std::path::Path::new(&path));
        println!(
            "{path}\n  reshade={} addons={} bitness={:?}  usable64={} usable32={}",
            found.is_reshade,
            found.has_addon_support,
            found.bitness,
            found.usable_for(64),
            found.usable_for(32)
        );
    }
}
