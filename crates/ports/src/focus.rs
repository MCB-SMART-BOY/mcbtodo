//! Durable focus checkpoints and reporting reads for the daemon timer.

use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimerState {
    Idle,
    Running,
    Paused,
    NeedsReview,
}

#[derive(Clone)]
pub struct FocusCheckpoint {
    pub id: Uuid,
    pub work_unit_id: Option<Uuid>,
    pub state: TimerState,
    pub started_at: DateTime<Utc>,
    pub stopped_at: Option<DateTime<Utc>>,
    pub elapsed: Duration,
}

pub struct FocusEntry {
    pub work_unit_id: Option<Uuid>,
    pub state: TimerState,
    pub elapsed: Duration,
}

/// Synchronous focus persistence for a single Daemon. The timer owns
/// monotonic elapsed-time accounting and only publishes state after a save
/// succeeds. A store recovers the last durable checkpoint before attachment.
pub trait FocusSessionStore {
    /// Saves the complete checkpoint by session identity. Implementations
    /// must update it atomically and reject a second active session.
    fn save(&self, checkpoint: &FocusCheckpoint) -> Result<()>;

    /// Visits persisted checkpoints whose session started on `date` in
    /// `timezone`. The visitor must not re-enter the repository.
    fn for_each_session_on(
        &self,
        date: NaiveDate,
        timezone: Tz,
        visit: &mut dyn FnMut(FocusEntry),
    ) -> Result<()>;
}
