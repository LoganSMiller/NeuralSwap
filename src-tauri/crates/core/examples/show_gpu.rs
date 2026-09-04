//! Print what graphics hardware this machine reports.
//!
//! `cargo run -p neuralswap-core --example show_gpu`
//!
//! Here for the same reason `scan_dir` and `list_library` are: generation
//! detection is inference from a driver description string, and the only way
//! to know it reads real hardware correctly is to point it at some.

fn main() {
    let all = neuralswap_core::platform::gpu::adapters();
    if all.is_empty() {
        println!("no display adapters could be read");
        return;
    }

    println!("{} adapter(s):", all.len());
    for adapter in &all {
        println!("\n  {}", adapter.name);
        println!(
            "    generation : {:?} ({})",
            adapter.generation,
            adapter.generation.label()
        );
        println!(
            "    driver     : {}{}",
            adapter.driver.as_deref().unwrap_or("unknown"),
            adapter
                .nvidia_driver
                .as_deref()
                .map(|nv| format!("  (NVIDIA {nv})"))
                .unwrap_or_default()
        );
    }

    match neuralswap_core::platform::gpu::best_nvidia() {
        Some(best) => {
            println!("\nDLSS decisions would be made against: {}", best.name);
            for floor in [
                neuralswap_core::platform::gpu::Generation::Turing,
                neuralswap_core::platform::gpu::Generation::Ada,
                neuralswap_core::platform::gpu::Generation::Blackwell,
            ] {
                println!(
                    "  at least {:?}? {}",
                    floor,
                    if best.generation.at_least(floor) {
                        "yes"
                    } else {
                        "no"
                    }
                );
            }
        }
        None => println!("\nno NVIDIA adapter found"),
    }
}
