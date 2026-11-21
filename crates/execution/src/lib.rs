//! Shared execution adapters used by the daemon and its local Worker process.

#[cfg(target_os = "linux")]
pub mod agent;
#[cfg(target_os = "linux")]
pub mod sandbox;
#[cfg(target_os = "linux")]
pub mod workflow_script;
