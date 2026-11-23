mod graph;
mod projection;
pub mod recurrence;
pub mod time;

use std::{fmt, str::FromStr};

pub use graph::{GraphError, TaskGraph};
pub use projection::{
    CalendarEntry, CalendarItem, CalendarKind, InboxSummary, Occurrence, OutlineItem,
};

use serde::{Deserialize, Serialize};
use time::{DateTimeSpan, Deadline, Span};

pub use uuid::Uuid as Id;

/// Namespace used to derive the executable Task created from an Inbox entry.
pub const INBOX_TASK_NAMESPACE: Id = Id::from_u128(0x6d6362746f646f000000000000000001);

#[derive(Debug, Deserialize, Serialize)]
pub struct Project {
    pub id: Id,
    #[serde(default)]
    pub status: ProjectStatus,
    pub active_period: Option<Span>,
    pub deadline: Option<Deadline>,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// A captured item that has not yet been turned into project work.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InboxEntry {
    pub id: Id,
    pub captured_at: chrono::DateTime<chrono::Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<Id>,
    #[serde(default)]
    pub tags: Vec<String>,
}

impl InboxEntry {
    /// Stable across retries and Vault restores.
    pub fn task_id(&self) -> Id {
        Id::new_v5(&INBOX_TASK_NAMESPACE, self.id.as_bytes())
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectStatus {
    Planned,
    #[default]
    Active,
    Completed,
    Cancelled,
    Archived,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Task {
    pub id: Id,
    pub project_id: Id,
    #[serde(default)]
    pub status: TaskStatus,
    #[serde(default)]
    pub priority: Priority,
    pub active_period: Option<Span>,
    pub deadline: Option<Deadline>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recurrence: Option<recurrence::CalendarRecurrence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occurrence: Option<TaskOccurrence>,
    #[serde(default)]
    pub tags: Vec<String>,
}

impl Task {
    pub fn new(id: Id, project_id: Id) -> Self {
        Self {
            id,
            project_id,
            status: TaskStatus::default(),
            priority: Priority::default(),
            active_period: None,
            deadline: None,
            recurrence: None,
            occurrence: None,
            tags: Vec::new(),
        }
    }
}

/// Identifies one persisted occurrence generated from a recurring Task template.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TaskOccurrence {
    pub template_id: Id,
    pub date: chrono::NaiveDate,
}

impl TaskOccurrence {
    /// Stable across retries, machines, and Vault restores.
    pub fn id(self) -> Id {
        Id::new_v5(&self.template_id, self.date.to_string().as_bytes())
    }
}

#[derive(Debug, Default, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    #[default]
    Todo,
    InProgress,
    WaitingReview,
    Completed,
    Cancelled,
    Archived,
}

#[derive(Debug, Default, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Low,
    #[default]
    Normal,
    High,
    Urgent,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct WorkUnit {
    pub id: Id,
    pub project_id: Id,
    pub parent_task_id: Id,
    #[serde(default)]
    pub status: WorkUnitStatus,
    #[serde(default)]
    pub priority: Priority,
    #[serde(default)]
    pub depends_on: Vec<Id>,
    pub estimated_minutes: Option<u32>,
    pub deadline: Option<Deadline>,
    #[serde(default)]
    pub time_blocks: Vec<TimeBlock>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_revision: Option<WorkflowRevisionHash>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<EvidenceRef>,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// The content hash of a published workflow revision.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(try_from = "String")]
pub struct WorkflowRevisionHash(String);

#[derive(Debug, thiserror::Error)]
#[error("expected a workflow revision hash of 64 lowercase hexadecimal digits")]
pub struct InvalidWorkflowRevisionHash;

impl TryFrom<String> for WorkflowRevisionHash {
    type Error = InvalidWorkflowRevisionHash;

    fn try_from(hash: String) -> Result<Self, Self::Error> {
        if hash.len() == 64
            && hash
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        {
            Ok(Self(hash))
        } else {
            Err(InvalidWorkflowRevisionHash)
        }
    }
}

impl FromStr for WorkflowRevisionHash {
    type Err = InvalidWorkflowRevisionHash;

    fn from_str(hash: &str) -> Result<Self, Self::Err> {
        hash.to_owned().try_into()
    }
}

impl WorkflowRevisionHash {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for WorkflowRevisionHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A persisted workflow step and any artifact recorded with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceRef {
    pub run_id: Id,
    pub step_index: u32,
}

/// A recorded transition of a WorkUnit to completed.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct WorkUnitCompleted {
    pub event_id: Id,
    pub work_unit_id: Id,
    pub project_id: Id,
    pub parent_task_id: Id,
    pub occurred_at: chrono::DateTime<chrono::Utc>,
    pub previous_status: WorkUnitStatus,
    /// The references present when the transition was recorded.
    pub evidence: Vec<EvidenceRef>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkUnitStatus {
    #[default]
    Todo,
    InProgress,
    WaitingReview,
    Completed,
    Cancelled,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    #[default]
    Busy,
    Free,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CalendarEvent {
    pub id: Id,
    pub when: Span,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recurrence: Option<recurrence::CalendarRecurrence>,
    #[serde(default)]
    pub availability: Availability,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct TimeBlock {
    pub when: DateTimeSpan,
    #[serde(default)]
    pub availability: Availability,
}
