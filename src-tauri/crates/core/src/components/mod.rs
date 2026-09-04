//! The third-party pieces an install is assembled from.
//!
//! Seven tools solve this problem seven ways, and the two extremes are both
//! wrong. Bundling everything produces a 231 MB download that is stale the day
//! it ships and quietly redistributes files nobody has the right to
//! redistribute. Fetching everything with no verification means whatever
//! arrives over the wire is written straight into a game folder.
//!
//! What is here instead:
//!
//! - [`catalog`] states, per component, who publishes it, under what licence,
//!   and therefore whether we may ship a copy. The licence is data, and the
//!   validator refuses a catalogue that pairs restricted terms with a source
//!   that would redistribute - so the rule is enforced rather than remembered.
//!
//! Three levels of confidence, named honestly rather than flattened into
//! "downloaded":
//!
//! | | what it means |
//! | --- | --- |
//! | `Bundled` | shipped by us; nothing to verify at runtime |
//! | `Pinned` | compared against a digest we published |
//! | `FirstUse` | recorded on first fetch, compared on every one after |
//! | `UserSupplied` | not fetched at all; found on the user's machine |
//!
//! `FirstUse` is the interesting one. A "latest release" cannot be pinned, so
//! the choice is between verifying nothing and detecting change. Recording the
//! digest the first time and comparing thereafter catches a release quietly
//! replaced later, which is the realistic supply-chain event for a moving
//! target - and which none of the tools in this space check at all.

pub mod catalog;

pub use catalog::{
    default_catalog, Catalog, Component, Licence, Role, Source, Trust, CATALOG_VERSION,
};
