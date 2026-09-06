//! Apply the OptiScaler settings to a real `OptiScaler.ini` and report the diff.
//!
//! Writes nothing. The point is to see, against a file somebody actually
//! tuned, that the edit touches the lines it means to and no others - a unit
//! test can only ever check the shapes it thought of, and a 1600-line file
//! with 38 sections has shapes nobody thought of.
//!
//! ```text
//! cargo run -p neuralswap-core --example check_ini -- "<path to OptiScaler.ini>"
//! ```

use std::fs;

use neuralswap_core::install::optiscaler::{self, Options};
use neuralswap_core::scan::api::Direct3D;
use neuralswap_core::scan::capability::{Feature, Substitute};

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        println!("usage: check_ini <path to OptiScaler.ini>");
        return;
    };
    let Ok(before) = fs::read_to_string(&path) else {
        println!("could not read {path}");
        return;
    };

    // The real decision path rather than a hand-written key list, so this
    // exercises what an install would actually write: a Direct3D 12 game with
    // its own DLSS, wanting the neural pass and frame generation.
    let settings = optiscaler::settings_for(
        &[Feature::NeuralRendering, Feature::FrameGeneration],
        None,
        Some(Substitute::FsrFrameGeneration),
        Some(Direct3D::Twelve),
        Options::default(),
    );
    for setting in &settings {
        println!(
            "  set    [{}] {} = {}",
            setting.section, setting.key, setting.value
        );
    }
    let after = optiscaler::write_into(&before, &settings);

    let old: Vec<&str> = before.lines().collect();
    let new: Vec<&str> = after.lines().collect();
    println!("{path}");
    println!("  lines  : {} -> {}", old.len(), new.len());
    println!("  endings: {} -> {}", endings(&before), endings(&after));

    // Line-by-line while the two stay in step, then report the tail. Enough
    // for an edit that only rewrites and inserts.
    let mut changed = 0usize;
    for (index, line) in new.iter().enumerate() {
        match old.get(index) {
            Some(was) if was == line => {}
            Some(was) => {
                changed += 1;
                if changed <= 12 {
                    println!("  {:>5} - {was}", index + 1);
                    println!("  {:>5} + {line}", index + 1);
                }
            }
            None => {
                changed += 1;
                if changed <= 12 {
                    println!("  {:>5} + {line}", index + 1);
                }
            }
        }
    }
    if changed > 12 {
        println!("  ... and {} more", changed - 12);
    }
    println!("  changed: {changed} line(s)");
}

fn endings(text: &str) -> String {
    let crlf = text.matches("\r\n").count();
    let lf = text.matches('\n').count() - crlf;
    format!("{crlf} CRLF, {lf} LF")
}
