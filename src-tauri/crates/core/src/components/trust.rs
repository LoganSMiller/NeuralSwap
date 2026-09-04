//! Remembering what a component's bytes were last time.
//!
//! A pinned source is easy: the digest ships with the catalogue and the bytes
//! either match or they do not. The hard case is a moving target - "the latest
//! release" - which cannot be pinned by definition. Every tool in this space
//! resolves that by verifying nothing at all, which means whatever the URL
//! serves today goes into a game folder.
//!
//! The alternative is trust on first use. Record the digest the first time a
//! component and version are fetched, and compare on every fetch after. That
//! cannot tell you the *first* download was genuine - nothing short of a
//! signature can - but it detects a published release being quietly replaced,
//! which is the realistic supply-chain event for a moving target.
//!
//! What happens on a mismatch is a judgement, and both extremes are wrong.
//! Refusing outright breaks a legitimate re-release, which does happen.
//! Accepting silently discards the only signal we had. So a change is
//! *reported* - the install stops and the user is told the publisher's bytes
//! differ from last time, with both digests - and it takes a deliberate
//! decision to go on.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{fail, Code, Result};
use crate::fsx::atomic::{read_to_string_or_none, write_json_atomic};

pub const TRUST_VERSION: u32 = 1;

/// What we recorded about one component version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Record {
    pub sha256: String,
    /// Where it came from, so a changed digest can be investigated.
    pub url: String,
    pub size: u64,
    /// When it was first seen, in milliseconds since the epoch.
    pub first_seen: i64,
    /// How many times it has been fetched and matched since.
    #[serde(default)]
    pub confirmations: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrustStore {
    pub version: u32,
    /// Keyed `<component id>@<version>`, so two versions of one component are
    /// two records rather than one that keeps changing.
    pub records: BTreeMap<String, Record>,
}

impl Default for TrustStore {
    fn default() -> Self {
        Self {
            version: TRUST_VERSION,
            records: BTreeMap::new(),
        }
    }
}

/// The answer to "have we seen these bytes before?".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "verdict")]
pub enum Verdict {
    /// Verified against a digest the catalogue shipped. The strongest answer.
    Pinned,
    /// Never seen this component version before. Nothing to compare against,
    /// so it is recorded and accepted - and the fact that it is a first
    /// sighting is worth saying, because it is the one fetch this scheme
    /// cannot vouch for.
    FirstSighting,
    /// Identical to what was recorded.
    Unchanged { confirmations: u32 },
    /// The publisher's bytes differ from last time.
    ///
    /// Not necessarily an attack - a re-tagged release does this - but not
    /// something to wave through either. Both digests travel with it so a
    /// person can go and look.
    Changed {
        recorded: String,
        recorded_first_seen: i64,
        found: String,
    },
}

impl Verdict {
    /// Whether an install may proceed on this without asking.
    ///
    /// A first sighting may: refusing it would mean nothing could ever be
    /// installed a first time. A change may not.
    pub const fn is_acceptable(&self) -> bool {
        matches!(
            self,
            Verdict::Pinned | Verdict::FirstSighting | Verdict::Unchanged { .. }
        )
    }

    pub fn explain(&self) -> String {
        match self {
            Verdict::Pinned => "verified against the digest NeuralSwap published".to_owned(),
            Verdict::FirstSighting => {
                "the first time this version has been downloaded, so there is nothing to \
                 compare it against - the digest has been recorded and will be checked from \
                 now on"
                    .to_owned()
            }
            Verdict::Unchanged { confirmations } => format!(
                "identical to the copy downloaded before ({confirmations} time(s) confirmed)"
            ),
            Verdict::Changed {
                recorded, found, ..
            } => format!(
                "the publisher is now serving different bytes for this version. Recorded \
                 {}, found {}. That can happen when a release is re-tagged, but it is not \
                 something to accept without looking.",
                short(recorded),
                short(found)
            ),
        }
    }
}

fn short(digest: &str) -> &str {
    digest.get(..12).unwrap_or(digest)
}

fn key(component: &str, version: &str) -> String {
    format!("{component}@{version}")
}

impl TrustStore {
    pub fn path_in(root: &Path) -> PathBuf {
        root.join("component-trust.json")
    }

    /// Read the record, or start empty.
    ///
    /// A damaged record is an error rather than a silent reset: forgetting
    /// every digest we ever recorded would turn every later fetch into a first
    /// sighting, which is exactly how this protection would quietly disappear.
    pub fn load(root: &Path) -> Result<Self> {
        let path = Self::path_in(root);
        let Some(text) = read_to_string_or_none(&path)? else {
            return Ok(Self::default());
        };
        let parsed: Self = serde_json::from_str(&text).map_err(|error| {
            crate::Error::new(
                Code::StateCorrupt,
                format!("could not parse {}: {error}", path.display()),
            )
        })?;
        if parsed.version > TRUST_VERSION {
            return fail(
                Code::StateVersionAhead,
                format!("{} was written by a newer build", path.display()),
            );
        }
        Ok(parsed)
    }

    pub fn save(&self, root: &Path) -> Result<()> {
        write_json_atomic(&Self::path_in(root), self)
    }

    pub fn get(&self, component: &str, version: &str) -> Option<&Record> {
        self.records.get(&key(component, version))
    }

    /// Compare a freshly downloaded digest against what is on record.
    ///
    /// Does not mutate: deciding and recording are separate so a caller can
    /// refuse a change without the refusal itself overwriting the evidence.
    pub fn check(&self, component: &str, version: &str, found: &str) -> Verdict {
        match self.get(component, version) {
            None => Verdict::FirstSighting,
            Some(record) if crate::hash::matches(&record.sha256, found) => Verdict::Unchanged {
                confirmations: record.confirmations,
            },
            Some(record) => Verdict::Changed {
                recorded: record.sha256.clone(),
                recorded_first_seen: record.first_seen,
                found: found.to_owned(),
            },
        }
    }

    /// Record a first sighting, or count a confirmation.
    ///
    /// Refuses to overwrite a digest that differs - that would erase the
    /// evidence a change ever happened. Replacing a record deliberately is
    /// [`Self::accept_change`], which is a separate and explicit act.
    pub fn remember(
        &mut self,
        component: &str,
        version: &str,
        digest: &str,
        url: &str,
        size: u64,
        now: i64,
    ) -> Result<()> {
        let entry = key(component, version);
        match self.records.get_mut(&entry) {
            Some(record) if crate::hash::matches(&record.sha256, digest) => {
                record.confirmations = record.confirmations.saturating_add(1);
                Ok(())
            }
            Some(record) => fail(
                Code::VerifyFailed,
                format!(
                    "{entry} is on record as {} and would be overwritten with {} - accept the \
                     change explicitly if that is intended",
                    short(&record.sha256),
                    short(digest)
                ),
            ),
            None => {
                self.records.insert(
                    entry,
                    Record {
                        sha256: digest.to_owned(),
                        url: url.to_owned(),
                        size,
                        first_seen: now,
                        confirmations: 0,
                    },
                );
                Ok(())
            }
        }
    }

    /// Replace a record after a user has decided a change is legitimate.
    ///
    /// Separate from `remember` so that accepting a changed release is always
    /// a deliberate call rather than something that can happen by falling
    /// through a branch.
    pub fn accept_change(
        &mut self,
        component: &str,
        version: &str,
        digest: &str,
        url: &str,
        size: u64,
        now: i64,
    ) {
        self.records.insert(
            key(component, version),
            Record {
                sha256: digest.to_owned(),
                url: url.to_owned(),
                size,
                first_seen: now,
                confirmations: 0,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "aa11bb22cc33dd44ee55ff66aa11bb22cc33dd44ee55ff66aa11bb22cc33dd44";
    const B: &str = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";

    fn store() -> TrustStore {
        TrustStore::default()
    }

    #[test]
    fn a_first_sighting_is_accepted_and_recorded() {
        let mut trust = store();
        assert_eq!(
            trust.check("dlss5-feeder", "0.12.0", A),
            Verdict::FirstSighting
        );
        assert!(Verdict::FirstSighting.is_acceptable());
        // And it says plainly that this is the fetch it cannot vouch for.
        assert!(Verdict::FirstSighting.explain().contains("nothing to"));

        trust
            .remember("dlss5-feeder", "0.12.0", A, "https://example.test/a", 10, 1)
            .expect("record it");
        assert_eq!(
            trust.check("dlss5-feeder", "0.12.0", A),
            Verdict::Unchanged { confirmations: 0 }
        );
    }

    #[test]
    fn the_same_bytes_again_counts_as_a_confirmation() {
        let mut trust = store();
        for _ in 0..3 {
            trust
                .remember("reshade", "6.8.0", A, "https://example.test/a", 10, 1)
                .expect("record");
        }
        assert_eq!(
            trust.check("reshade", "6.8.0", A),
            Verdict::Unchanged { confirmations: 2 }
        );
    }

    #[test]
    fn a_publisher_serving_different_bytes_is_reported_not_waved_through() {
        // The event this whole module exists to catch, and the one nothing
        // else in this space checks for.
        let mut trust = store();
        trust
            .remember("lumenite", "main", A, "https://example.test/a", 10, 1_700)
            .expect("record");

        let verdict = trust.check("lumenite", "main", B);
        assert_eq!(
            verdict,
            Verdict::Changed {
                recorded: A.to_owned(),
                recorded_first_seen: 1_700,
                found: B.to_owned(),
            }
        );
        assert!(!verdict.is_acceptable(), "a change must stop the install");
        // Both digests are in the message, so somebody can go and look.
        assert!(verdict.explain().contains(&A[..12]));
        assert!(verdict.explain().contains(&B[..12]));
    }

    #[test]
    fn remembering_a_different_digest_refuses_rather_than_erasing_the_evidence() {
        let mut trust = store();
        trust
            .remember("lumenite", "main", A, "https://example.test/a", 10, 1)
            .expect("record");

        let refused = trust
            .remember("lumenite", "main", B, "https://example.test/a", 10, 2)
            .expect_err("must not silently overwrite");
        assert_eq!(refused.code, Code::VerifyFailed);
        // The original record survives, which is the point.
        assert_eq!(
            trust.get("lumenite", "main").map(|r| r.sha256.as_str()),
            Some(A)
        );
    }

    #[test]
    fn accepting_a_change_is_a_separate_deliberate_act() {
        let mut trust = store();
        trust
            .remember("lumenite", "main", A, "https://example.test/a", 10, 1)
            .expect("record");
        trust.accept_change("lumenite", "main", B, "https://example.test/b", 20, 99);

        assert_eq!(
            trust.check("lumenite", "main", B),
            Verdict::Unchanged { confirmations: 0 }
        );
        let record = trust.get("lumenite", "main").expect("record");
        assert_eq!(record.first_seen, 99, "the clock restarts on a new record");
        assert_eq!(record.size, 20);
    }

    #[test]
    fn two_versions_of_one_component_are_two_records() {
        // Otherwise upgrading a component would look like a changed release
        // every single time.
        let mut trust = store();
        trust
            .remember("reshade", "6.8.0", A, "https://example.test/a", 1, 1)
            .expect("record");
        trust
            .remember("reshade", "6.7.3", B, "https://example.test/b", 1, 1)
            .expect("record");

        assert_eq!(
            trust.check("reshade", "6.8.0", A),
            Verdict::Unchanged { confirmations: 0 }
        );
        assert_eq!(
            trust.check("reshade", "6.7.3", B),
            Verdict::Unchanged { confirmations: 0 }
        );
        assert_eq!(trust.records.len(), 2);
    }

    #[test]
    fn the_record_survives_a_round_trip_to_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut trust = TrustStore::load(dir.path()).expect("empty is fine");
        assert!(trust.records.is_empty());

        trust
            .remember("reshade", "6.8.0", A, "https://example.test/a", 42, 7)
            .expect("record");
        trust.save(dir.path()).expect("save");

        let revived = TrustStore::load(dir.path()).expect("load");
        assert_eq!(revived, trust);
        assert_eq!(
            revived.check("reshade", "6.8.0", A),
            Verdict::Unchanged { confirmations: 0 }
        );
    }

    #[test]
    fn a_damaged_record_is_an_error_rather_than_a_silent_reset() {
        // Forgetting every digest would turn every later fetch into a first
        // sighting, which is how this protection would quietly disappear.
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(TrustStore::path_in(dir.path()), b"{ not json").expect("write");
        assert_eq!(
            TrustStore::load(dir.path()).err().map(|error| error.code),
            Some(Code::StateCorrupt)
        );
    }

    #[test]
    fn a_record_from_a_newer_build_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ahead = TrustStore {
            version: TRUST_VERSION + 1,
            ..Default::default()
        };
        ahead.save(dir.path()).expect("save");
        assert_eq!(
            TrustStore::load(dir.path()).err().map(|error| error.code),
            Some(Code::StateVersionAhead)
        );
    }

    #[test]
    fn digest_comparison_ignores_hex_case() {
        let mut trust = store();
        trust
            .remember("reshade", "6.8.0", A, "https://example.test/a", 1, 1)
            .expect("record");
        assert_eq!(
            trust.check("reshade", "6.8.0", &A.to_uppercase()),
            Verdict::Unchanged { confirmations: 0 }
        );
    }
}
