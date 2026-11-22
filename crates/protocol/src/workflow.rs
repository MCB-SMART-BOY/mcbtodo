use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use mcbtodo_workflow::{
    run::{ObservedOutcome, RunResolution, RunState, RunSummary},
    schedule::{Fold, Missed, Overlap, ScheduleSummary, Trigger},
};

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
        /// Required for approval. Older clients that omit it cannot approve.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        review: Option<String>,
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
