//! Content hashing.
//!
//! Every question this application needs to answer about a DLL is a question
//! about its bytes: is the file already the one the package offers, did the
//! copy land intact, is this still the file we installed or has something
//! replaced it since. Size and modification time cannot answer any of them.
//!
//! Timestamps in particular are actively misleading here. Windows `CopyFile`
//! preserves the source timestamp, so a runtime taken out of an NVIDIA package
//! and dropped into a game folder looks *older* than the game around it. That
//! was tried first while building the provenance detection, and it is why the
//! comparisons that matter are all hashes.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::error::{fail, Code, Result};

/// 64 KiB. Large enough that the syscall overhead disappears against the hash
/// itself, small enough to stay in cache and to keep a cancelled scan
/// responsive.
const CHUNK: usize = 64 * 1024;

/// Lower-case hex, which is the form the plans, manifests and vectors all use.
pub fn hash_bytes(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex(&hasher.finalize())
}

/// Hash a file by streaming it, so a 200 MB runtime does not become a 200 MB
/// allocation.
pub fn hash_file(path: &Path) -> Result<String> {
    let mut file = File::open(path).map_err(|error| {
        crate::Error::new(
            Code::PeUnreadable,
            format!("could not open {}: {error}", path.display()),
        )
    })?;
    hash_reader(&mut file).map_err(|error| {
        crate::Error::new(
            Code::PeUnreadable,
            format!("could not read {}: {error}", path.display()),
        )
    })
}

/// Hash and count in one pass. The length is returned because every caller
/// that wants a hash also wants to know it read the number of bytes it
/// expected - a short read is how a truncated copy passes for a good one.
pub fn hash_reader<R: Read>(source: &mut R) -> io::Result<String> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; CHUNK];
    loop {
        let read = source.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        // `read` is bounded by the buffer length, so the slice cannot panic.
        hasher.update(&buffer[..read]);
    }
    Ok(hex(&hasher.finalize()))
}

/// Compare a hash to an expectation, case-insensitively.
///
/// Constant time is not a requirement: these are content digests of files the
/// user already has, not secrets, and there is nothing here for a timing
/// attacker to learn.
pub fn matches(actual: &str, expected: &str) -> bool {
    actual.eq_ignore_ascii_case(expected)
}

/// Raise the mismatch as an error, with both hashes in the message. Truncated
/// to twelve characters: enough to identify which file went wrong in a log,
/// without turning a diagnostic line into two 64-character hex strings.
pub fn verify(path: &Path, actual: &str, expected: &str) -> Result<()> {
    if matches(actual, expected) {
        return Ok(());
    }
    fail(
        Code::VerifyFailed,
        format!(
            "{} hashed {} but the plan expected {}",
            path.display(),
            short(actual),
            short(expected)
        ),
    )
}

fn short(hash: &str) -> &str {
    hash.get(..12).unwrap_or(hash)
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        // Writing to a String cannot fail; the Result is discarded knowingly
        // rather than unwrapped, because a panic here would be a lie.
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_published_sha256_test_vectors() {
        // The canonical empty-input and "abc" digests from FIPS 180-4. If this
        // ever disagrees, the problem is the wiring, not the algorithm.
        assert_eq!(
            hash_bytes(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hash_bytes(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn streaming_agrees_with_hashing_in_one_go() {
        // Deliberately not a multiple of the chunk size, so the last read is
        // short - the case where an off-by-one in the buffer slice would show.
        let data: Vec<u8> = (0..(CHUNK * 2 + 12345)).map(|i| (i % 251) as u8).collect();
        let mut cursor = std::io::Cursor::new(&data);
        assert_eq!(hash_reader(&mut cursor).expect("hash"), hash_bytes(&data));
    }

    #[test]
    fn a_file_hashes_the_same_as_its_bytes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("runtime.dll");
        let data = vec![0x42_u8; CHUNK + 7];
        std::fs::write(&file, &data).expect("write");
        assert_eq!(hash_file(&file).expect("hash"), hash_bytes(&data));
    }

    #[test]
    fn an_unreadable_file_is_an_error_not_an_empty_hash() {
        let dir = tempfile::tempdir().expect("tempdir");
        let outcome = hash_file(&dir.path().join("absent.dll"));
        assert_eq!(
            outcome.err().map(|error| error.code),
            Some(Code::PeUnreadable)
        );
    }

    #[test]
    fn comparison_ignores_hex_case_but_nothing_else() {
        let digest = hash_bytes(b"abc");
        assert!(matches(&digest, &digest.to_uppercase()));
        assert!(!matches(&digest, &hash_bytes(b"abd")));
    }

    #[test]
    fn a_mismatch_names_the_file_and_both_hashes() {
        let error = verify(
            Path::new("game/nvngx_dlss.dll"),
            &hash_bytes(b"a"),
            &hash_bytes(b"b"),
        )
        .expect_err("should not match");
        assert_eq!(error.code, Code::VerifyFailed);
        assert!(error.detail.contains("nvngx_dlss.dll"));
    }
}
