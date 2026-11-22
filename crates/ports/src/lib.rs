//! Stable business-facing ports for replaceable mcbtodo providers.
//!
//! This crate contains provider contracts and business-shaped values. It does
//! not depend on an operating-system transport implementation, a database
//! client, or a provider-specific protocol.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

use mcbtodo_domain::WorkUnitCompleted;
use serde::{Deserialize, Deserializer, Serialize, de};
use thiserror::Error;

pub mod focus;
pub mod notifications;
pub mod process;
pub mod sandbox;
pub mod transport;
pub mod workflow;

pub use mcbtodo_workflow::Scalar;

/// A provider's runtime health is separate from its platform support claim.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderHealth {
    /// The adapter will check availability during dispatch.
    Unchecked,
    Healthy,
    Degraded,
    Unavailable,
}

/// Describes capabilities and declared limits without granting authorization.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderMetadata {
    pub provider_id: String,
    pub provider_version: String,
    pub protocol_version: String,
    pub capabilities: BTreeSet<String>,
    pub supported_platforms: BTreeSet<String>,
    pub required_permissions: BTreeSet<String>,
    pub resource_limits: BTreeMap<String, u64>,
    pub health: ProviderHealth,
}

impl ProviderMetadata {
    /// Create metadata with no declared limits and no observed health result.
    pub fn new(
        provider_id: impl Into<String>,
        provider_version: impl Into<String>,
        protocol_version: impl Into<String>,
    ) -> Self {
        Self {
            provider_id: provider_id.into(),
            provider_version: provider_version.into(),
            protocol_version: protocol_version.into(),
            capabilities: BTreeSet::new(),
            supported_platforms: BTreeSet::new(),
            required_permissions: BTreeSet::new(),
            resource_limits: BTreeMap::new(),
            health: ProviderHealth::Unchecked,
        }
    }
}

/// A provider request expressed entirely in business values.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentRequest {
    pub capability: String,
    pub revision: String,
    pub inputs: BTreeMap<String, Scalar>,
}

/// A normalized provider result. The host still checks the keys and types
/// against the workflow's declared output ports before committing.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentResult {
    #[serde(deserialize_with = "AgentResult::deserialize_outputs")]
    pub outputs: BTreeMap<String, Scalar>,
}

impl AgentResult {
    /// Decode a provider's scalar output object directly from its wire
    /// representation, where duplicate keys can still be detected.
    pub fn deserialize_outputs<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<BTreeMap<String, Scalar>, D::Error> {
        struct Outputs;

        impl<'de> de::Visitor<'de> for Outputs {
            type Value = BTreeMap<String, Scalar>;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("an object with unique scalar output fields")
            }

            fn visit_map<A: de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut outputs = BTreeMap::new();
                while let Some((key, value)) = map.next_entry()? {
                    if outputs.insert(key, value).is_some() {
                        return Err(de::Error::custom("duplicate Agent output"));
                    }
                }
                Ok(outputs)
            }
        }

        deserializer.deserialize_map(Outputs)
    }
}

#[cfg(test)]
mod agent_result_tests {
    use super::{AgentResult, Scalar};

    #[test]
    fn outputs_preserve_scalar_types_and_reject_ambiguous_json() {
        let result = serde_json::from_str::<AgentResult>(
            r#"{"outputs":{"flag":false,"count":9223372036854775807,"title":"计划"}}"#,
        )
        .unwrap();
        assert_eq!(
            result.outputs,
            [
                ("flag".into(), Scalar::Boolean(false)),
                ("count".into(), Scalar::Integer(i64::MAX)),
                ("title".into(), Scalar::String("计划".into())),
            ]
            .into()
        );
        for invalid in [
            r#"{"outputs":{"value":1,"value":2}}"#,
            r#"{"outputs":{"value":9223372036854775808}}"#,
            r#"{"outputs":{"value":1.0}}"#,
            r#"{"outputs":{"value":null}}"#,
            r#"{"outputs":{"value":[]}}"#,
            r#"{"outputs":{"value":{}}}"#,
        ] {
            assert!(
                serde_json::from_str::<AgentResult>(invalid).is_err(),
                "{invalid}"
            );
        }
    }
}

/// Cancellation and deadline state supplied by the host.
#[derive(Clone, Copy)]
pub struct AgentExecutionContext<'a> {
    pub deadline: Instant,
    pub cancellation: &'a dyn Cancellation,
}

/// Host-owned cancellation state.
pub trait Cancellation: Send + Sync {
    fn is_cancelled(&self) -> bool;
}

impl Cancellation for AtomicBool {
    fn is_cancelled(&self) -> bool {
        self.load(Ordering::Acquire)
    }
}

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("Agent call was cancelled")]
    Cancelled,
    #[error("Agent call timed out")]
    TimedOut,
    #[error("{0}")]
    Unavailable(String),
    #[error("{0}")]
    Rejected(String),
    #[error("{0}")]
    InvalidResult(String),
    #[error("{0}")]
    Transport(String),
    /// Dispatch may have reached the provider, but no valid result is known.
    #[error("Agent outcome is uncertain: {0}")]
    OutcomeUnknown(String),
}

/// A replaceable Agent implementation selected by the host.
///
/// Calls are synchronous. Implementations must honor the supplied deadline
/// and cancellation state and bound the bytes they accept. Once dispatch
/// might have reached the provider, a timeout, cancellation, or protocol
/// failure must return `OutcomeUnknown`. Before dispatch, `TimedOut` and
/// `Cancelled` report a call that did not reach the provider.
/// The host authorizes the capability and revision before calling `execute`.
/// Adapters enforce their supported capabilities at the dispatch boundary.
pub trait AgentProvider: Send + Sync {
    fn metadata(&self) -> &ProviderMetadata;

    fn execute(
        &self,
        request: AgentRequest,
        context: AgentExecutionContext<'_>,
    ) -> Result<AgentResult, AgentError>;
}

/// Delivers a single plain-text desktop notification on the host's dispatch
/// thread. The caller owns queueing and retries. A failure may leave delivery
/// uncertain, and success acknowledges submission rather than display.
pub trait NotificationSink: Send + 'static {
    fn deliver(&self, title: &str, body: &str, timeout: Duration) -> Result<(), NotificationError>;
}

#[derive(Debug, Error)]
pub enum NotificationError {
    #[error("{0}")]
    Unavailable(String),
    #[error("notification helper timed out")]
    TimedOut,
    #[error("{0}")]
    DeliveryFailed(String),
}

/// A batch of committed facts and their local read cursors.
/// The cursor is scoped to one journal and is not an event identity.
pub type CompletionPage = Vec<(i64, WorkUnitCompleted)>;

/// Reads committed WorkUnit completions for the host's workflow scheduler.
pub trait CompletionEventStore: Send + Sync + 'static {
    /// Returns the latest committed cursor, or zero for an empty journal.
    /// Registration uses this on the event loop, so lock contention must
    /// return `Busy` immediately.
    fn watermark(&self) -> Result<i64, CompletionReadError>;

    /// Returns up to 64 committed facts in ascending cursor order, excluding
    /// `after`. The background reader can wait on storage I/O but must report
    /// writer-lock contention as `Busy` so the host can retry the same cursor.
    fn page(&self, after: i64) -> Result<CompletionPage, CompletionReadError>;
}

#[derive(Debug, Error)]
pub enum CompletionReadError {
    #[error("completion event journal is busy")]
    Busy,
    #[error("{0}")]
    Storage(String),
}
