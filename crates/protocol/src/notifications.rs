use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::workflow::RunState;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationPage {
    /// Newest terminal transitions first.
    pub notifications: Vec<WorkflowNotification>,
    pub next_before: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowNotification {
    pub run_id: Uuid,
    pub revision: String,
    pub schedule_id: Option<Uuid>,
    pub state: RunState,
    pub finished_at: DateTime<Utc>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReminderNotificationPage {
    /// Newest persisted inbox entries first.
    pub notifications: Vec<ReminderNotification>,
    pub next_before: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReminderNotification {
    pub id: Uuid,
    pub reminder_id: Uuid,
    pub target_id: Option<Uuid>,
    pub title: String,
    pub scheduled_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}
