//! Stateful/concurrent nesting engine: NfpCache, GA, placement, rayon dispatch,
//! progress events. See RUST-REWRITE-PLAN.md Phase 3-5.

pub mod audit;
pub mod banded;
pub mod benchmark_log;
pub mod cache;
pub mod cache_key;
pub mod consolidation;
pub mod dispatch;
pub mod ga;
pub mod pattern;
pub mod placement;
pub mod profile;
pub mod repack;
pub mod spread;
