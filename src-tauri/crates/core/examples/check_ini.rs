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

use neuralswap_core::install::ini;

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        println!("usage: check_ini <path to OptiScaler.ini>");
        return;
    };
    let Ok(before) = fs::read_to_string(&path) else {
        println!("could not read {path}");
        return;
    };

    // What the OptiScaler route writes to turn FSR 3.1 frame generation on.
    let after = ini::set(
        &before,
        "FrameGen",
        &[
            ("Enabled", "true"),
            ("FGInput", "upscaler"),
            ("FGOutput", "fsrfg"),
        ],
    );
    let after = ini::set(&after, "OptiFG", &[("HUDFix", "true")]);

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
