//! SQLite adapters for mcbtodo.
//!
//! Connections remain private to their stores. The daemon owns the workflow,
//! focus and notification stores. Vault owns the completion journal and its lock.

pub mod completion_journal;
#[cfg(unix)]
pub mod focus;
#[cfg(unix)]
pub mod run_store;
#[cfg(unix)]
mod schema_version;
#[cfg(unix)]
pub mod workspace_snapshot;
