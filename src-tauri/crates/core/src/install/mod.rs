//! Installing a runtime into a game folder.
//!
//! The subsystem is split so that the part which *decides* has no ability to
//! write, and the part which writes makes no decisions:
//!
//! - [`plan`] derives what an install would do, purely, from data. This is
//!   what a user is shown and what they agree to.
//! - [`recover`] decides what to do about a journal an interrupted install
//!   left behind, purely, from what survived.
//! - [`version`] orders the version strings the other two compare.
//! - [`preflight`] runs every check it has before an install starts, so a user
//!   sees the whole situation at once rather than one obstacle at a time.
//! - [`journal`] writes the intent down before anything is touched, and can
//!   undo what was done - whether that undo happens a second later or after a
//!   power cut and a reboot.
//! - [`manifest`] remembers what we installed and where the displaced
//!   originals went, which is what makes provenance a fact rather than an
//!   inference and what makes an install reversible months later.
//! - [`apply`] walks a plan. It is the only code that writes into a game
//!   folder, and it makes no decisions of its own.
//! - [`restore`] undoes an install, using the manifest and the backup store.
//!   It needs no journal because every step of it is idempotent, so an
//!   interrupted restore is repaired by running it again.
//!
//! Both decision modules are held to the behavioural vectors in `spec/`, so a
//! reimplementation can be shown to reach the same verdicts on the same
//! inputs. That matters more here than anywhere else in the core: these are
//! the rules that govern somebody else's game folder.

pub mod apply;
pub mod discover;
pub mod journal;
pub mod manifest;
pub mod package;
pub mod placement;
pub mod plan;
pub mod preflight;
pub mod recipe;
pub mod recover;
pub mod restore;
pub mod version;

pub use apply::{apply, Applied, Outcome, Reached};
pub use discover::{best_for, from_driver_store, from_game, rank, Candidate, Origin};
pub use journal::{Journal, JournalRecord, JournalStep, RecoveryOutcome};
pub use manifest::{FileStatus, InstallManifest, Integrity};
pub use package::{read_package, read_present};
pub use plan::{build_plan, Plan, PlanInput, Route, Step, StepAction, StepReason, Warning};
pub use preflight::{preflight, Check, CheckName, CheckOutcome, Preflight};
pub use recover::{decide_recovery, JournalState, Recovery, RecoveryDecision};
pub use restore::{preview, restore};
pub use version::{compare_versions, format_version, parse_version, relate, VersionRelation};
