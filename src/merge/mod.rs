//! Conflict detection and AI-assisted merging of overlapping mod files.

pub mod ai;
pub mod ai_cache;
pub mod base_game;
pub mod bundle;
pub mod collection;
pub mod conflicts;
pub mod external;
pub mod file_merge;
pub mod ignore_rules;
pub mod inline_scripts;
pub mod lint;
pub mod order_check;
pub mod patch;
pub mod scan_cache;
pub mod script_check;
pub mod snapshot;
pub mod source;
pub mod trivial;
pub mod unused;
pub mod update_all;
pub mod validate;
pub mod vocabulary;
