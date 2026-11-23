//! Shared execution adapters used by the daemon and its local Worker process.

#[cfg(target_os = "linux")]
pub mod agent;
#[cfg(target_os = "linux")]
pub mod codex_host;
#[cfg(target_os = "linux")]
mod model_relay;
#[cfg(target_os = "linux")]
pub mod notification;
#[cfg(target_os = "linux")]
mod plugin_agent;
#[cfg(target_os = "linux")]
pub mod sandbox;
#[cfg(target_os = "linux")]
mod unix_agent;
#[cfg(target_os = "linux")]
pub mod workflow_script;

#[cfg(target_os = "linux")]
pub use mcbtodo_platform::codex_config;
