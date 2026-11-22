//! Durable notification Inbox and reminder delivery contracts.

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use mcbtodo_workflow::run::RunState;

pub const REMINDER_MAX_ATTEMPTS: u32 = 5;

#[derive(Debug)]
pub struct DueReminder {
    pub reminder_id: Uuid,
    pub target_id: Option<Uuid>,
    pub title: String,
    pub scheduled_at: DateTime<Utc>,
}

pub struct ReminderDelivery {
    pub id: Uuid,
    pub title: String,
}

/// Synchronous local notification storage, called serially by the daemon.
/// The host owns the notification sink and calls recovery only at startup
/// under exclusive daemon ownership, before issuing new delivery claims.
pub trait NotificationStore {
    /// Returns the last durable terminal transition cursor, or zero.
    fn workflow_watermark(&self) -> Result<i64>;

    /// Returns the newest `limit` transitions strictly after `after`,
    /// ordered from oldest to newest within that batch.
    fn recent_workflow_completions(
        &self,
        after: i64,
        limit: u32,
    ) -> Result<Vec<(i64, Uuid, RunState)>>;

    /// Publishes previously unseen `(reminder_id, scheduled_at)` occurrences
    /// and their pending delivery rows atomically. Returns only new entries.
    fn publish_reminders(
        &self,
        due: &[DueReminder],
        now: DateTime<Utc>,
    ) -> Result<Vec<ReminderNotification>>;

    /// Makes interrupted deliveries with budget remaining eligible again.
    /// Leaves exhausted deliveries in the failed state. A delivery whose
    /// acknowledgement was lost may be displayed more than once.
    fn recover_reminder_deliveries(&self) -> Result<()>;

    /// Claims up to `limit` due deliveries and charges one attempt each.
    fn claim_reminder_deliveries(
        &self,
        now: DateTime<Utc>,
        limit: usize,
        lease_until: DateTime<Utc>,
    ) -> Result<Vec<ReminderDelivery>>;

    /// Acknowledges a claim or records failure for bounded backoff, up to
    /// [`REMINDER_MAX_ATTEMPTS`]. An ID without an active claim is a no-op.
    fn finish_reminder_delivery(
        &self,
        id: Uuid,
        result: std::result::Result<(), &str>,
        now: DateTime<Utc>,
    ) -> Result<()>;

    /// Releases a claim and refunds the attempt. Call only when the host
    /// knows the notification did not enter the delivery queue.
    fn release_reminder_delivery(&self, id: Uuid) -> Result<()>;

    /// Pages the workflow Inbox newest first, excluding the supplied run.
    fn workflow_page(&self, before: Option<Uuid>) -> Result<NotificationPage>;

    /// Pages the reminder Inbox newest first, excluding the supplied entry.
    fn reminder_page(&self, before: Option<Uuid>) -> Result<ReminderNotificationPage>;
}

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
