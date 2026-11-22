//! Business state for durable workflow runs.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunSummary {
    pub id: Uuid,
    pub revision: String,
    pub due_at: Option<DateTime<Utc>>,
    pub schedule_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_run_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_event_id: Option<Uuid>,
    pub state: RunState,
    pub current_node: Option<String>,
    pub step_count: u32,
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<RunResolution>,
}

impl RunSummary {
    /// Identifies the exact waiting decision and its recorded review text.
    pub fn approval_review(&self) -> Option<String> {
        if self.state != RunState::Waiting {
            return None;
        }
        let node = self.current_node.as_deref()?;
        let detail = self.detail.as_deref()?;
        let mut hash = Sha256::new();
        hash.update(b"mcbtodo.workflow-approval.v1\0");
        hash.update(self.id.as_bytes());
        hash.update(self.step_count.to_be_bytes());
        for field in [self.revision.as_str(), node, detail] {
            hash.update((field.len() as u64).to_be_bytes());
            hash.update(field.as_bytes());
        }
        Some(hex::encode(hash.finalize()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservedOutcome {
    Completed,
    Failed,
    NotStarted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunResolution {
    pub outcome: ObservedOutcome,
    pub note: String,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Pending,
    Running,
    Waiting,
    Succeeded,
    Failed,
    Cancelling,
    Cancelled,
    OutcomeUnknown,
}
