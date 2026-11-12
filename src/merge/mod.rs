//! Conflict detection and AI-assisted merging of overlapping mod files.

pub mod ai;
pub mod bundle;
pub mod collection;
pub mod conflicts;
pub mod external;
pub mod file_merge;
pub mod ignore_rules;
pub mod lint;
pub mod patch;
pub mod snapshot;
pub mod source;
pub mod scan_cache;
pub mod trivial;
