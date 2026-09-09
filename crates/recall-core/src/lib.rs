//! Deterministic, single-owner storage and command semantics.
//! Time is supplied by the caller; this crate performs no I/O or thread creation.

pub mod command;
mod expiry;
mod store;

pub use command::{parse, Command, CommandError, Operation, ParseLimits};
pub use store::{EngineError, Prepared, Shard, ShardLimits, ShardStats};

