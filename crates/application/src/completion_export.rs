use anyhow::{Result, ensure};
use chrono::{DateTime, Utc};
use mcbtodo_domain::{EvidenceRef, Id, WorkUnitCompleted, WorkUnitStatus};
use mcbtodo_vault::{CompletionEventRecord, VaultStore};
use serde::{Deserialize, Serialize};

/// A logical completion fact accompanied by its journal-local pagination cursor.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompletionExportEntry {
    sequence: i64,
    envelope: CompletionEnvelope,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompletionEnvelope {
    schema: CompletionSchema,
    event_type: CompletionEventType,
    event_id: Id,
    idempotency_key: Id,
    #[serde(skip_serializing_if = "Option::is_none")]
    origin_device_id: Option<Id>,
    entity_id: Id,
    created_at: DateTime<Utc>,
    observed_revision: String,
    payload: CompletionPayload,
}

#[derive(Serialize, Deserialize)]
enum CompletionSchema {
    #[serde(rename = "mcbtodo.completion-event/v1")]
    V1,
}

#[derive(Serialize, Deserialize)]
enum CompletionEventType {
    #[serde(rename = "work_unit.completed")]
    WorkUnitCompleted,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompletionPayload {
    project_id: Id,
    parent_task_id: Id,
    previous_status: WorkUnitStatus,
    evidence: Vec<EvidenceRef>,
}

impl CompletionExportEntry {
    /// Replays the event under the receiving Vault's consistency checks.
    /// The sequence belongs to the exporting journal and is ignored.
    pub fn import_into(self, store: &VaultStore) -> Result<bool> {
        let envelope = self.envelope;
        ensure!(
            envelope.idempotency_key == envelope.event_id,
            "completion idempotency_key must equal event_id"
        );
        let event = WorkUnitCompleted {
            event_id: envelope.event_id,
            work_unit_id: envelope.entity_id,
            project_id: envelope.payload.project_id,
            parent_task_id: envelope.payload.parent_task_id,
            occurred_at: envelope.created_at,
            previous_status: envelope.payload.previous_status,
            evidence: envelope.payload.evidence,
        };
        Ok(store.import_work_unit_completion(
            &event,
            &envelope.observed_revision,
            envelope.origin_device_id,
        )?)
    }
}

impl From<CompletionEventRecord> for CompletionExportEntry {
    fn from(record: CompletionEventRecord) -> Self {
        let WorkUnitCompleted {
            event_id,
            work_unit_id,
            project_id,
            parent_task_id,
            occurred_at,
            previous_status,
            evidence,
        } = record.event;
        Self {
            sequence: record.sequence,
            envelope: CompletionEnvelope {
                schema: CompletionSchema::V1,
                event_type: CompletionEventType::WorkUnitCompleted,
                event_id,
                idempotency_key: event_id,
                origin_device_id: record.origin_device_id,
                entity_id: work_unit_id,
                created_at: occurred_at,
                observed_revision: record.observed_revision,
                payload: CompletionPayload {
                    project_id,
                    parent_task_id,
                    previous_status,
                    evidence,
                },
            },
        }
    }
}
