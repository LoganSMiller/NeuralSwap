//! A guard on the strings this crate shows people.
//!
//! Rust lets a string literal span lines, and the leading indentation of the
//! continuation line becomes part of the string unless the line before it ends
//! in a backslash. Forget the backslash and the code compiles, the tests pass,
//! and a user reads a sentence with eighteen spaces in the middle of it.
//!
//! That has happened here more than once, always the same way: a scripted edit
//! that mangled the escape. Two shipped before this test existed. It costs a
//! few milliseconds to make the next one a build failure instead.

use std::fs;
use std::path::PathBuf;

/// Long enough that no one typed it deliberately inside a sentence.
const SUSPICIOUS_RUN: &str = "      ";

/// Every `.rs` file under the crate's `src`.
fn sources() -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                found.push(path);
            }
        }
    }
    found
}

/// Whether `line` is inside prose rather than code we would not judge.
///
/// Comments are exempt: a table in a doc comment is aligned with runs of
/// spaces on purpose, and that is the whole point of it.
fn is_comment(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("//") || trimmed.starts_with("*")
}

#[test]
fn no_user_facing_string_has_a_run_of_spaces_in_it() {
    let mut offenders: Vec<String> = Vec::new();

    for path in sources() {
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        // Nothing below `#[cfg(test)]` is shown to anyone, and it is full of
        // fixtures whose spacing is the data being tested - Steam's VDF format
        // separates a key from its value with a run of whitespace.
        let prose = text.split("#[cfg(test)]").next().unwrap_or(&text);

        for (number, line) in prose.lines().enumerate() {
            if is_comment(line) {
                continue;
            }
            // Only within a string literal, and only a run long enough to be a
            // lost continuation rather than deliberate spacing. A mangled
            // continuation carries the indentation of the line it was on, so
            // it is never short - the two real ones were fourteen and eighteen
            // spaces. Six keeps a deliberate gap in a fixture out of it.
            let Some(open) = line.find('"') else { continue };
            let rest = &line[open + 1..];
            let literal = rest.rfind('"').map_or(rest, |close| &rest[..close]);
            if literal.contains(SUSPICIOUS_RUN) {
                offenders.push(format!(
                    "{}:{}",
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("?"),
                    number + 1
                ));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "these string literals contain a run of spaces, which is what a line \
         continuation missing its backslash looks like: {}",
        offenders.join(", ")
    );
}

#[test]
fn the_guard_can_actually_see_a_lost_continuation() {
    // A test that only ever passes proves nothing. This is the shape of the
    // two real ones it was written for, and a comment line, which it must not
    // flag - a doc-comment table is aligned with spaces deliberately.
    let bad = r#"            "and this                  game is Direct3D 11.","#;
    let good = r#"            "and this game is Direct3D 11.","#;
    let table = r#"    /// sl.dlss_g     1000     d3d12, vk"#;
    // A fixture whose spacing is the data, which must not be flagged: three
    // spaces separating a VDF key from its value is the format, not a mistake.
    let fixture = r#"    "key"   "value""#;

    let flagged = |line: &str| {
        if is_comment(line) {
            return false;
        }
        line.find('"').is_some_and(|open| {
            let rest = &line[open + 1..];
            rest.rfind('"')
                .map_or(rest, |close| &rest[..close])
                .contains(SUSPICIOUS_RUN)
        })
    };

    assert!(flagged(bad), "the guard missed a lost continuation");
    assert!(!flagged(good), "the guard flagged a correct string");
    assert!(!flagged(table), "the guard flagged an aligned comment");
    assert!(!flagged(fixture), "the guard flagged a deliberate gap");
}

/// Keeps the walker honest: a zero-file walk would make the test above vacuous.
#[test]
fn the_source_walk_finds_the_crate() {
    let found = sources();
    assert!(
        found.len() > 20,
        "expected the crate's sources, found {} file(s)",
        found.len()
    );
    assert!(found.iter().any(|path| path.ends_with("capability.rs")));
}
