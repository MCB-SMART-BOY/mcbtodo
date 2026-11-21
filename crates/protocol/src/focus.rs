//! Derived daily focus totals. Each session's entire duration belongs to its start
//! date in the requested timezone, including work after midnight or after resuming.
//! Elapsed time uses the daemon's monotonic clock on Linux, which excludes system
//! suspend. A daemon restart preserves only the durable checkpoint for a running
//! session and marks it for review.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DailyFocusReport {
    pub date: NaiveDate,
    pub timezone: String,
    /// Whole seconds rounded down after summing precise durations.
    pub total_elapsed_seconds: u64,
    pub session_count: u64,
    pub needs_review_count: u64,
    pub by_work_unit: Vec<WorkUnitFocus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkUnitFocus {
    pub work_unit_id: Option<Uuid>,
    /// Whole seconds rounded down within this group.
    pub elapsed_seconds: u64,
    pub session_count: u64,
    pub needs_review_count: u64,
}
