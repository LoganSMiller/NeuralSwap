pub mod api;
pub mod candidates;
pub mod folder;
pub mod integration;

pub use api::{detect, Api, Verdict, MARKERS};
pub use candidates::{is_probably_not_a_game, should_skip_content, should_skip_dir, SKIP_DIRS};
pub use folder::{
    scan_folder, Candidate, EmptyReason, FolderScan, RuntimeFile, RuntimeKind, RULES,
};
pub use integration::{assess, Assessment, Integration, Route};
