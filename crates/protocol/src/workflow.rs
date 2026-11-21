use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkflowRequest {
    Enqueue {
        run_id: Uuid,
        revision: String,
        due_at: Option<DateTime<Utc>>,
    },
    Inspect {
        run_id: Uuid,
    },
    List {
        before: Option<Uuid>,
    },
    Step {
        run_id: Uuid,
        index: u32,
    },
    Artifact {
        run_id: Uuid,
        index: u32,
        offset: u32,
    },
    Decide {
        run_id: Uuid,
        node: String,
        approved: bool,
    },
    Cancel {
        run_id: Uuid,
    },
    ResolveUnknown {
        run_id: Uuid,
        outcome: ObservedOutcome,
        note: String,
    },
    AuthorizeScript {
        revision: String,
        node: String,
        granted: bool,
    },
    AddSchedule {
        id: Uuid,
        revision: String,
        trigger: Trigger,
        missed: Missed,
        overlap: Overlap,
    },
    InspectSchedule {
        id: Uuid,
    },
    DisableSchedule {
        id: Uuid,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkflowResponse {
    Run(RunSummary),
    Runs {
        runs: Vec<RunSummary>,
        next_before: Option<Uuid>,
    },
    Step {
        step: serde_json::Value,
    },
    Artifact {
        bytes: Vec<u8>,
        next_offset: Option<u32>,
    },
    ScriptAuthorization {
        revision: String,
        node: String,
        granted: bool,
    },
    Schedule(ScheduleSummary),
    Resolution(RunResolution),
}

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Trigger {
    Interval {
        anchor: DateTime<Utc>,
        seconds: u32,
    },
    Cron {
        expression: String,
        timezone: String,
        fold: Fold,
    },
    /// Queue on future successes of this exact published revision.
    Dependency {
        upstream_revision: String,
    },
    /// Queue on future committed completion events for this WorkUnit in the default Vault.
    WorkUnitCompleted {
        work_unit_id: Uuid,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fold {
    Earlier,
    Later,
    Both,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "policy", rename_all = "snake_case", deny_unknown_fields)]
pub enum Missed {
    Skip,
    RunOnce,
    ReplayLimited { limit: u8 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Overlap {
    Skip,
    Queue,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleSummary {
    pub id: Uuid,
    pub revision: String,
    pub trigger: Trigger,
    pub missed: Missed,
    pub overlap: Overlap,
    pub next_at: Option<DateTime<Utc>>,
    pub enabled: bool,
    pub missed_occurrences: u64,
    pub overlap_skipped: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cascade_skipped: u64,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
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
