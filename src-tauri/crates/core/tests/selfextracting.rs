// Tests may panic freely: a broken fixture should abort the run, loudly.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

//! Reading an archive that has something in front of it.
//!
//! ReShade ships as a self-extracting executable: an installer followed by an
//! appended ZIP. Two things about that shape break a naive reader, and both
//! were found by pointing this at the real 6.8.0 installer rather than by
//! reasoning about the specification.
//!
//! 1. Its end-of-central-directory record declares a comment length of zero
//!    but is followed by about 8 KB of trailing data. A reader that identifies
//!    the record by checking that the comment accounts for exactly the
//!    remaining bytes rejects the file. That check is why the upstream project
//!    shells out to BSD `tar` for this one file.
//! 2. The central directory's local-header offsets are relative to the start
//!    of the *archive*, not the file, so they have to be shifted by the size
//!    of the executable in front of it.
//!
//! The synthetic cases below pin both behaviours on every platform. The test
//! against the real installer runs only where somebody has one.

use std::fs;
use std::path::{Path, PathBuf};

use neuralswap_core::zip::extract::{extract_zip, Limits};

/// Build a small, valid ZIP in memory using the same helper the vectors use.
///
/// Rather than hand-assemble one here, a known-good archive is taken from the
/// generated fixtures - it is byte-identical to what the reference
/// implementation produced, so any failure is about the prefix rather than
/// about a hand-written header.
fn benign_archive() -> Vec<u8> {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../spec/zip/benign.zip.bin")
        .canonicalize()
        .expect("spec/zip/benign.zip.bin - run `npm run vectors`");
    fs::read(fixture).expect("read the benign fixture")
}

fn extract_bytes(bytes: &[u8], name: &str) -> neuralswap_core::Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("ns-sfx-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("scratch dir");
    let archive = dir.join("input.bin");
    fs::write(&archive, bytes).expect("write archive");
    let out = dir.join("out");
    extract_zip(&archive, &out, Limits::default()).map(|_| out)
}

#[test]
fn an_archive_with_an_executable_in_front_of_it_still_reads() {
    // The shape of a self-extracting installer: arbitrary bytes, then a ZIP
    // whose internal offsets are all relative to where the ZIP begins.
    let mut bytes = vec![0x4d, 0x5a]; // an MZ header, as a real one would have
    bytes.resize(64 * 1024, 0x90);
    bytes.extend_from_slice(&benign_archive());

    let out = extract_bytes(&bytes, "prefixed").expect("a prefixed archive should read");
    assert!(
        out.join("readme.txt").is_file(),
        "the prefix shift was not applied to the entry offsets"
    );
    assert_eq!(
        fs::read_to_string(out.join("readme.txt")).expect("read"),
        "hello world"
    );
    assert!(out.join("bin/tool.dll").is_file());
    assert!(out.join("bin/raw.bin").is_file());
}

#[test]
fn trailing_bytes_after_the_record_do_not_hide_it() {
    // ReShade's exact quirk: comment length says zero, yet the file continues.
    let mut bytes = benign_archive();
    bytes.extend_from_slice(&[0xab; 8_000]);

    let out = extract_bytes(&bytes, "trailing").expect("trailing bytes should not hide the record");
    assert!(out.join("readme.txt").is_file());
}

#[test]
fn both_quirks_at_once() {
    let mut bytes = vec![0x4d, 0x5a];
    bytes.resize(4096, 0x90);
    bytes.extend_from_slice(&benign_archive());
    bytes.extend_from_slice(&[0xcd; 8_000]);

    let out = extract_bytes(&bytes, "both").expect("a real installer's shape");
    assert!(out.join("readme.txt").is_file());
    assert!(out.join("bin/tool.dll").is_file());
}

#[test]
fn a_file_that_is_not_an_archive_at_all_is_still_refused() {
    // Loosening how the record is found must not loosen what counts as an
    // archive. These are the vectors' own hostile cases, restated here
    // because this change is what could plausibly weaken them.
    let junk = vec![0x7a_u8; 4096];
    assert!(extract_bytes(&junk, "junk").is_err());

    let mut truncated = benign_archive();
    truncated.truncate(truncated.len() - 40);
    assert!(extract_bytes(&truncated, "truncated").is_err());

    // The signature bytes alone, with nothing behind them.
    let mut fake = vec![0u8; 512];
    fake.extend_from_slice(&[0x50, 0x4b, 0x05, 0x06]);
    fake.extend_from_slice(&[0u8; 18]);
    // Claims one entry, so the reader must look for a central directory and
    // find nothing.
    fake[512 + 10] = 1;
    assert!(extract_bytes(&fake, "fake").is_err());
}

/// The real thing, where it is present. Reads the archive only - nothing is
/// installed, and the extracted copy goes to a temp directory.
#[test]
fn the_real_reshade_installer_reads_if_present() {
    let candidates = [
        "C:\\Users\\user\\Downloads\\ReShade_Setup_6.8.0_Addon.exe",
        "C:\\Users\\user\\Downloads\\ReShade_Setup_6.8.0.exe",
    ];
    let Some(installer) = candidates.iter().map(Path::new).find(|path| path.is_file()) else {
        // Said out loud, because a conditional test that passes by skipping
        // is worth nothing and should not look like a pass.
        eprintln!("SKIPPED: no ReShade installer on this machine");
        return;
    };
    eprintln!("reading the real installer: {}", installer.display());

    let dir = std::env::temp_dir().join(format!("ns-reshade-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let limits = Limits {
        // A ReShade installer carries both architectures and the shader set.
        max_total_bytes: 256 * 1024 * 1024,
        max_entries: 4096,
        ..Limits::default()
    };

    let extracted = extract_zip(installer, &dir, limits)
        .unwrap_or_else(|error| panic!("the real installer should read: {error}"));

    // What every install route actually needs out of it.
    let names: Vec<String> = extracted
        .files
        .iter()
        .map(|rel| rel.to_ascii_lowercase())
        .collect();
    assert!(
        names.iter().any(|name| name.contains("reshade64.dll")),
        "no ReShade64.dll among {} entries: {:?}",
        names.len(),
        names.iter().take(20).collect::<Vec<_>>()
    );

    eprintln!(
        "read {} entries, {} bytes; found ReShade64.dll",
        extracted.files.len(),
        extracted.bytes
    );
    let _ = fs::remove_dir_all(&dir);
}
