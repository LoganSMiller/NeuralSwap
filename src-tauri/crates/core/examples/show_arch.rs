//! Which architectures a runtime carries code for.

fn main() {
    let card = neuralswap_core::platform::gpu::best_nvidia();
    let generation = card.as_ref().map(|found| found.generation);
    println!(
        "this card: {}",
        card.as_ref()
            .map(|found| found.name.clone())
            .unwrap_or_else(|| "not detected".to_owned())
    );

    for path in std::env::args().skip(1) {
        let path = std::path::Path::new(&path);
        let archs = neuralswap_core::platform::fatbin::architectures(path);
        println!("\n{}", path.display());
        println!("  records: {archs:?}");
        println!(
            "  verdict: {:?}",
            neuralswap_core::platform::fatbin::check(path, generation)
        );
    }
}
