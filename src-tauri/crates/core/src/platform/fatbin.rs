//! Which GPU architectures a runtime actually carries code for.
//!
//! `nvngx_dlssnr.dll` is compiled per architecture. NVIDIA's build targets
//! Blackwell; the community re-targeted it for Ada, Ampere and Turing. Install
//! one that has no code for the card and the feature fails at creation with
//! nothing on disk to explain why - the file is present, the right size, and
//! signed.
//!
//! So the file is asked directly. A CUDA fatbinary records the architecture of
//! every kernel image it holds, and those records are readable without running
//! anything.
//!
//! # Why this is the authority and a name table is not
//!
//! A table mapping "the RTX 40 build" to `sm_89` is a claim about a filename.
//! The records are a fact about the bytes. DLSS5-Autopilot's `gpu.py` makes
//! the same point about its own table: the fatbin check "is still the
//! authority - these only decide the [preference]", precisely because a
//! patched build can be named anything.
//!
//! # The format
//!
//! Verified against two independent readings - DLSS5-Autopilot's `gpu.py` and
//! the neural-upstream project's `FINDINGS.md`, which rebuilt the runtime for
//! Ada and documents the layout it had to parse:
//!
//! ```text
//! fatbinary header, at the magic 0xBA55ED50
//!   +6   u16   header size
//!   +8   u64   total size of the entries that follow
//! entry header, 64 bytes
//!   +4   u32   this entry's header size
//!   +8   u64   payload size
//!   +16  u64   compressed size
//!   +28  u32   architecture (the sm number)
//! ```
//!
//! The two sources disagree slightly: `FINDINGS.md` names offset 28 outright,
//! while `gpu.py` probes 24, 28 and 20 in turn and takes the first value that
//! looks like a known architecture. 28 is used here as the documented offset,
//! with the same "does it look like a known architecture" guard, so a record
//! that does not parse is skipped rather than believed.
//!
//! **A fatbinary can hold several architectures.** That is not obvious from
//! the format and it is the thing that has to be got right: NVIDIA's own
//! build has one architecture per fatbinary, so a reader that stops at the
//! first entry looks correct against it and is wrong against every
//! multi-architecture community build. Measured on the development machine:
//! Cyberpunk's runtime reports `{120: 30}` and Ready or Not's reports
//! `{75: 15, 86: 15, 89: 15, 120: 23}` - one file, fifteen fatbinaries, four
//! architectures.
//!
//! # What it cannot tell you
//!
//! An architecture is necessary, not sufficient. A GTX 1660 and an RTX 2060
//! are both `sm_75`, and only one of them has the tensor cores these kernels
//! need - so the hardware gate in [`crate::scan::capability`] still applies.
//! This answers "does this file contain code for that architecture", which is
//! a different question from "will that card run it".

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::platform::gpu::Generation;

const MAGIC: [u8; 4] = 0xBA55_ED50_u32.to_le_bytes();

/// Architectures worth recognising, with what they are.
///
/// The list is a filter, not a claim: a record whose architecture is not here
/// is treated as unparsed rather than as a new card, because the alternative
/// is reading a payload offset as an architecture and reporting nonsense.
const KNOWN: [(u32, &str); 15] = [
    (50, "Maxwell"),
    (52, "Maxwell"),
    (53, "Maxwell"),
    (60, "Pascal"),
    (61, "GTX 10 (Pascal)"),
    (62, "Pascal"),
    (70, "Volta"),
    (72, "Xavier"),
    (75, "RTX 20 / GTX 16 (Turing)"),
    (80, "A100 (Ampere)"),
    (86, "RTX 30 (Ampere)"),
    (87, "Orin"),
    (89, "RTX 40 (Ada Lovelace)"),
    (90, "H100 (Hopper)"),
    (120, "RTX 50 (Blackwell)"),
];

fn label_of(sm: u32) -> String {
    KNOWN
        .iter()
        .find(|(known, _)| *known == sm)
        .map_or_else(|| format!("sm_{sm}"), |(_, name)| (*name).to_owned())
}

/// The architecture a generation needs code for.
///
/// `None` where the question does not arise: a card older than Turing cannot
/// run these kernels whatever the file holds, and an unidentified adapter has
/// no architecture to check against.
pub const fn sm_for(generation: Generation) -> Option<u32> {
    match generation {
        Generation::Turing | Generation::TuringNoRt => Some(75),
        Generation::Ampere => Some(86),
        Generation::Ada => Some(89),
        Generation::Blackwell => Some(120),
        // A newer card than this build knows about. Its architecture number
        // cannot be guessed, and guessing wrong would refuse a file that
        // works.
        Generation::NewerThanKnown
        | Generation::PreTuring
        | Generation::NotNvidia
        | Generation::Unknown => None,
    }
}

/// Whether a runtime will run on a card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "verdict")]
pub enum Compatibility {
    /// The file carries code for this card.
    Runs { carries: Vec<String> },
    /// It does not, and it will fail at feature creation.
    WillNotRun { needs: String, carries: Vec<String> },
    /// Could not be determined - an unreadable file, no records found, or a
    /// card with no architecture to check against.
    ///
    /// Never treated as a failure. A check we cannot run is not evidence of a
    /// problem, and refusing an install on that basis would block perfectly
    /// good machines - the same rule the preflight checks follow.
    Unknown { why: String },
}

/// Read the architectures a file carries code for.
///
/// Streamed rather than loaded: the neural rendering runtime is around 158 MB
/// and only its headers matter. A sliding window with an overlap the size of
/// one entry header means a magic value straddling a read boundary is still
/// found.
pub fn architectures(path: &Path) -> Vec<u32> {
    const CHUNK: usize = 1 << 20;
    /// Enough to hold a fatbinary header plus the first entry header.
    const OVERLAP: usize = 128;

    let Ok(mut file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    // A second handle for the header reads.
    //
    // They seek, and the scan below reads sequentially. Sharing one handle
    // would leave the scan's position wherever the last header happened to
    // be, so it would skip forward unpredictably and miss most of the file -
    // while still finding enough records to look like it worked.
    let Ok(mut headers) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let length = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    let mut found: Vec<u32> = Vec::new();
    let mut buffer = vec![0_u8; CHUNK + OVERLAP];
    let mut base: u64 = 0;
    let mut carried = 0_usize;

    loop {
        let read = match file.read(&mut buffer[carried..]) {
            Ok(0) => 0,
            Ok(count) => count,
            Err(_) => break,
        };
        let filled = carried + read;
        if filled == 0 {
            break;
        }

        for hit in memchr::memmem::find_iter(&buffer[..filled], &MAGIC) {
            found.extend(read_fatbin(&mut headers, base + hit as u64, length));
        }

        if read == 0 {
            break;
        }
        // Carry the tail forward so a magic split across the boundary is seen.
        let keep = OVERLAP.min(filled);
        buffer.copy_within(filled - keep..filled, 0);
        base += (filled - keep) as u64;
        carried = keep;
    }

    found.sort_unstable();
    found.dedup();
    found
}

/// Every architecture in the fatbinary at `at`.
///
/// **All of its entries, not the first.** An earlier version read one entry
/// per fatbinary, on the assumption that a fatbinary holds images for a single
/// architecture. That is false, and the file that disproved it was sitting on
/// the development machine: Ready or Not's `nvngx_dlssnr.dll` is a
/// multi-architecture build carrying Turing, Ampere, Ada *and* Blackwell.
/// Reading the first entry saw only Turing and declared the file unable to run
/// on a Blackwell card - a confident, specific, wrong refusal of an install
/// that would have worked.
///
/// Entries are walked by their own declared sizes: header size at `+4`,
/// payload size at `+8`, and the next entry begins after both.
fn read_fatbin(file: &mut std::fs::File, at: u64, length: u64) -> Vec<u32> {
    /// A stop on a record that does not parse, so a corrupt or unexpected
    /// layout cannot turn into an unbounded walk.
    const MOST_ENTRIES: usize = 4096;

    let mut header = [0_u8; 16];
    if file.seek(SeekFrom::Start(at)).is_err() || file.read_exact(&mut header).is_err() {
        return Vec::new();
    }

    let header_size = u64::from(u16::from_le_bytes([header[6], header[7]]));
    let total = u64::from_le_bytes([
        header[8], header[9], header[10], header[11], header[12], header[13], header[14],
        header[15],
    ]);
    // A header smaller than the fields it must contain is not one, and a
    // declared span longer than the file is not one either.
    if header_size < 16 || total == 0 || total > length {
        return Vec::new();
    }

    let mut found = Vec::new();
    let mut at = at + header_size;
    let end = at + total;
    for _ in 0..MOST_ENTRIES {
        if at + 32 > end {
            break;
        }
        let mut entry = [0_u8; 32];
        if file.seek(SeekFrom::Start(at)).is_err() || file.read_exact(&mut entry).is_err() {
            break;
        }

        let entry_header = u64::from(u32::from_le_bytes([entry[4], entry[5], entry[6], entry[7]]));
        let payload = u64::from_le_bytes([
            entry[8], entry[9], entry[10], entry[11], entry[12], entry[13], entry[14], entry[15],
        ]);
        // The bounds that keep a misread from walking off: an entry header is
        // 64 bytes in these files, and neither field may be absurd.
        if !(24..=4096).contains(&entry_header) || payload == 0 || payload > length {
            break;
        }

        let sm = u32::from_le_bytes([entry[28], entry[29], entry[30], entry[31]]);
        // The guard that makes a wrong offset harmless: a payload length read
        // as an architecture is not in the table, so it is skipped rather
        // than reported.
        if KNOWN.iter().any(|(known, _)| *known == sm) {
            found.push(sm);
        }
        at += entry_header + payload;
    }
    found
}

/// Ask a runtime file whether it will run on this card.
pub fn check(path: &Path, generation: Option<Generation>) -> Compatibility {
    let carries = architectures(path);
    if carries.is_empty() {
        return Compatibility::Unknown {
            why: format!(
                "no architecture records were found in {}",
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string())
            ),
        };
    }
    let listed: Vec<String> = carries.iter().copied().map(label_of).collect();

    let Some(needs) = generation.and_then(sm_for) else {
        return Compatibility::Unknown {
            why: "this card's architecture is not known, so the file cannot be checked \
                  against it"
                .to_owned(),
        };
    };

    if carries.contains(&needs) {
        Compatibility::Runs { carries: listed }
    } else {
        Compatibility::WillNotRun {
            needs: label_of(needs),
            carries: listed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One entry: a 64-byte header carrying the architecture, then a payload.
    ///
    /// The header's own size and its payload's size are both written, because
    /// that is how the walk finds the next entry - and an earlier fixture
    /// left them zero, which the reader that only looked at the first entry
    /// never noticed.
    fn entry(sm: u32, payload: usize) -> Vec<u8> {
        const HEADER: u32 = 64;
        let mut bytes = vec![0_u8; HEADER as usize];
        bytes[4..8].copy_from_slice(&HEADER.to_le_bytes());
        bytes[8..16].copy_from_slice(&(payload as u64).to_le_bytes());
        bytes[28..32].copy_from_slice(&sm.to_le_bytes());
        bytes.extend(std::iter::repeat_n(0xAB_u8, payload));
        bytes
    }

    /// One fatbinary holding an entry per architecture given.
    fn fatbin(archs: &[u32]) -> Vec<u8> {
        let entries: Vec<u8> = archs.iter().flat_map(|&sm| entry(sm, 48)).collect();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&MAGIC);
        bytes.extend_from_slice(&[0, 0]); // +4 version
        bytes.extend_from_slice(&16_u16.to_le_bytes()); // +6 header size
        bytes.extend_from_slice(&(entries.len() as u64).to_le_bytes()); // +8
        bytes.extend_from_slice(&entries);
        bytes
    }

    fn write_temp(bytes: &[u8]) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().expect("tempfile");
        std::io::Write::write_all(&mut file, bytes).expect("write");
        std::io::Write::flush(&mut file).expect("flush");
        file
    }

    /// A file holding one fatbinary per architecture - the shape NVIDIA's own
    /// single-architecture build has.
    fn fatbins(archs: &[u32]) -> tempfile::NamedTempFile {
        let mut bytes = Vec::new();
        // Some leading content, so nothing depends on a record being first.
        bytes.extend_from_slice(b"MZ....this is a PE.....");
        for &sm in archs {
            bytes.extend_from_slice(&fatbin(&[sm]));
        }
        write_temp(&bytes)
    }

    #[test]
    fn the_architectures_are_read_from_the_records() {
        let file = fatbins(&[89, 120]);
        assert_eq!(architectures(file.path()), vec![89, 120]);
    }

    #[test]
    fn a_build_for_the_wrong_card_is_reported_as_such() {
        // The failure this prevents: the file is present, the right size and
        // signed, and the feature fails at creation with nothing on disk to
        // explain it.
        let blackwell_only = fatbins(&[120]);
        match check(blackwell_only.path(), Some(Generation::Ada)) {
            Compatibility::WillNotRun { needs, carries } => {
                assert!(needs.contains("Ada"), "{needs}");
                assert_eq!(carries, vec!["RTX 50 (Blackwell)".to_owned()]);
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_matching_build_runs() {
        let ada = fatbins(&[89]);
        match check(ada.path(), Some(Generation::Ada)) {
            Compatibility::Runs { carries } => {
                assert_eq!(carries, vec!["RTX 40 (Ada Lovelace)".to_owned()]);
            }
            other => panic!("expected a pass, got {other:?}"),
        }
    }

    #[test]
    fn a_multi_architecture_build_runs_on_any_of_them() {
        let multi = fatbins(&[75, 86, 89, 120]);
        for generation in [
            Generation::Turing,
            Generation::Ampere,
            Generation::Ada,
            Generation::Blackwell,
        ] {
            assert!(
                matches!(
                    check(multi.path(), Some(generation)),
                    Compatibility::Runs { .. }
                ),
                "{generation:?}"
            );
        }
    }

    #[test]
    fn every_entry_of_a_multi_architecture_fatbin_is_read() {
        // The bug this pins, found on the development machine rather than in
        // a fixture. Ready or Not's `nvngx_dlssnr.dll` is one file whose
        // fatbins each carry Turing, Ampere, Ada *and* Blackwell. An earlier
        // version read the first entry of each, saw only Turing, and declared
        // the file unable to run on a Blackwell card - a confident, specific,
        // wrong refusal of an install that would have worked.
        //
        // The old fixture could not have caught it: it put one architecture
        // in each fatbin, which is exactly the assumption the code made.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"MZ....");
        bytes.extend_from_slice(&fatbin(&[75, 86, 89, 120]));
        let file = write_temp(&bytes);

        assert_eq!(architectures(file.path()), vec![75, 86, 89, 120]);
        for generation in [Generation::Blackwell, Generation::Turing, Generation::Ada] {
            assert!(
                matches!(
                    check(file.path(), Some(generation)),
                    Compatibility::Runs { .. }
                ),
                "{generation:?}"
            );
        }
    }

    #[test]
    fn an_unreadable_file_is_unknown_rather_than_a_refusal() {
        // A check we cannot run is not evidence of a problem. Refusing on this
        // basis would block installs on perfectly good machines, which is the
        // rule every preflight check follows.
        match check(Path::new("no-such-file.dll"), Some(Generation::Ada)) {
            Compatibility::Unknown { why } => assert!(!why.is_empty()),
            other => panic!("expected unknown, got {other:?}"),
        }
    }

    #[test]
    fn a_file_with_no_records_is_unknown() {
        let mut file = tempfile::NamedTempFile::new().expect("tempfile");
        std::io::Write::write_all(&mut file, b"not a fatbinary anywhere in here").expect("write");
        match check(file.path(), Some(Generation::Ada)) {
            Compatibility::Unknown { why } => assert!(why.contains("no architecture")),
            other => panic!("expected unknown, got {other:?}"),
        }
    }

    #[test]
    fn a_card_with_no_known_architecture_is_unknown_not_refused() {
        let ada = fatbins(&[89]);
        for generation in [
            None,
            Some(Generation::Unknown),
            Some(Generation::NewerThanKnown),
            Some(Generation::PreTuring),
        ] {
            assert!(
                matches!(check(ada.path(), generation), Compatibility::Unknown { .. }),
                "{generation:?}"
            );
        }
    }

    #[test]
    fn a_record_that_does_not_parse_is_skipped_rather_than_believed() {
        // The guard that makes a wrong offset harmless. A payload length read
        // as an architecture is not in the table, so it contributes nothing
        // instead of inventing a card.
        // 999 is not an architecture anybody ships.
        let file = write_temp(&fatbin(&[999]));
        assert!(architectures(file.path()).is_empty());
    }

    #[test]
    fn a_record_straddling_a_read_boundary_is_still_found() {
        // The sliding window's whole purpose. A 158 MB file is read in
        // chunks, and a magic value landing across a boundary must not be
        // missed - a missed record means "no architectures found", which reads
        // as unknown and silently drops the check.
        let mut bytes = vec![0_u8; (1 << 20) - 2];
        bytes.extend_from_slice(&fatbin(&[89]));
        let file = write_temp(&bytes);

        assert_eq!(architectures(file.path()), vec![89]);
    }

    #[test]
    fn turing_without_tensor_cores_still_maps_to_its_architecture() {
        // A GTX 1660 and an RTX 2060 are both sm_75, and only one has the
        // tensor cores these kernels need. This answers the architecture
        // question; the hardware gate in `capability` answers the other one,
        // and conflating them would either refuse a working card or accept an
        // impossible one.
        assert_eq!(sm_for(Generation::TuringNoRt), Some(75));
        assert_eq!(sm_for(Generation::Turing), Some(75));
    }
}
