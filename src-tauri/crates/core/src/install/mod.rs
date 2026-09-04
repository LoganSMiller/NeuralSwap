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
//!
//! Both decision modules are held to the behavioural vectors in `spec/`, so a
//! reimplementation can be shown to reach the same verdicts on the same
//! inputs. That matters more here than anywhere else in the core: these are
//! the rules that govern somebody else's game folder.

pub mod plan;
pub mod recover;
pub mod version;

pub use plan::{build_plan, Plan, PlanInput, Route, Step, StepAction, StepReason, Warning};
pub use recover::{decide_recovery, JournalState, Recovery, RecoveryDecision};
pub use version::{compare_versions, format_version, parse_version, relate, VersionRelation};
