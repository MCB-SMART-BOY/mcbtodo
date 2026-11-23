//! Typed client and wire protocol for the local workspace daemon.

mod client;
pub mod focus;
pub mod notifications;
pub mod wire;
pub mod worker;
pub mod workflow;

pub use client::request;
pub use mcbtodo_ports::focus::TimerState;
pub use wire::ProtocolError;

use chrono::{DateTime, FixedOffset, NaiveDate, Utc};
use mcbtodo_domain::{
    CalendarItem, DependencyOperation, InboxSummary, OutlineItem, WorkUnitDependencyPreview,
    WorkUnitGraphNode, time::Deadline,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Status,
    /// Starts a new session or resumes the current paused session.
    /// Omit the work unit to start an unassigned session or resume the existing work unit.
    Start {
        work_unit_id: Option<Uuid>,
    },
    Pause,
    Stop,
    FocusReport {
        date: NaiveDate,
        /// IANA timezone used to determine the local session start date.
        timezone: String,
    },
    /// Project the default Vault over an inclusive window of at most 366 days.
    Calendar {
        start: NaiveDate,
        through: NaiveDate,
        /// IANA timezone used to determine which local dates an occurrence overlaps.
        timezone: String,
    },
    /// Project the default Vault hierarchy from one validated snapshot.
    Outline {
        timezone: String,
    },
    /// Read the editable deadline and revision from the same default Vault snapshot.
    DeadlineContext {
        id: Uuid,
    },
    /// Set or clear a deadline using the digest observed by the client.
    SetDeadline {
        id: Uuid,
        revision: String,
        deadline: Option<Deadline>,
    },
    /// List captured Inbox entries from one validated default Vault snapshot.
    Inbox,
    /// Project the dependency graph from one validated default Vault snapshot.
    WorkUnitGraph {
        timezone: String,
    },
    PreviewWorkUnitDependencyChange {
        target: Uuid,
        sources: std::collections::BTreeSet<Uuid>,
        operation: DependencyOperation,
        timezone: String,
    },
    SetWorkUnitDependencies {
        target: Uuid,
        prerequisites: Vec<Uuid>,
        revision: String,
    },
    /// Assign an observed Inbox entry to a Project and retain its source document.
    TriageInbox {
        inbox_id: Uuid,
        project_id: Uuid,
        inbox_revision: String,
    },
    /// List terminal workflow notifications, starting strictly before the given run ID.
    ListNotifications {
        before: Option<Uuid>,
    },
    /// List reminder deliveries, starting strictly before the given notification ID.
    ListReminderNotifications {
        before: Option<Uuid>,
    },
    /// Capture a Task under a Project in the daemon's default Vault.
    CaptureTask {
        task_id: Uuid,
        project_id: Uuid,
        /// Digest of the Project revision observed when opening the capture form.
        project_revision: String,
        title: String,
    },
    /// Create a busy, non-recurring all-day event in the daemon's default Vault.
    CreateAllDayEvent {
        event_id: Uuid,
        title: String,
        start: NaiveDate,
        through: NaiveDate,
    },
    /// Create a Project with an active period or a date-only deadline.
    CreatePlannedProject {
        project_id: Uuid,
        title: String,
        dates: ProjectDates,
    },
    /// Create a one-time reminder in the daemon's default Vault.
    CreateReminder {
        reminder_id: Uuid,
        target_id: Option<Uuid>,
        title: String,
        at: DateTime<FixedOffset>,
    },
    /// Append a busy time block to a WorkUnit in the daemon's default Vault.
    CreateTimeBlock {
        work_unit_id: Uuid,
        /// Digest of the WorkUnit revision observed when selecting it.
        work_unit_revision: String,
        start: DateTime<FixedOffset>,
        end: DateTime<FixedOffset>,
    },
    Workflow(workflow::WorkflowRequest),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProjectDates {
    ActivePeriod {
        start: NaiveDate,
        through: NaiveDate,
    },
    Deadline {
        due: NaiveDate,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Response {
    Status(TimerStatus),
    FocusReport(focus::DailyFocusReport),
    Calendar {
        items: Vec<CalendarItem>,
    },
    /// One or more ordered frames on a single Outline connection.
    /// The typed client assembles all frames before returning the response.
    Outline {
        items: Vec<OutlineItem>,
        more: bool,
    },
    DeadlineContext {
        revision: String,
        deadline: Option<Deadline>,
    },
    DeadlineSet {
        synced: bool,
    },
    /// Ordered frames on one Inbox connection, assembled by the typed client.
    Inbox {
        items: Vec<InboxSummary>,
        more: bool,
    },
    /// Ordered graph rows assembled by the typed client before edges can be followed.
    WorkUnitGraph {
        items: Vec<WorkUnitGraphNode>,
        more: bool,
    },
    WorkUnitDependencyPreview(WorkUnitDependencyPreview),
    WorkUnitDependenciesSet {
        synced: bool,
    },
    /// The Inbox assignment is visible. `synced` describes its durability.
    InboxTriaged {
        synced: bool,
    },
    Notifications(notifications::NotificationPage),
    ReminderNotifications(notifications::ReminderNotificationPage),
    /// The Task is visible. `synced` is false if its durability is uncertain.
    TaskCaptured {
        task_id: Uuid,
        synced: bool,
    },
    /// The event is visible. `synced` is false if its durability is uncertain.
    AllDayEventCreated {
        event_id: Uuid,
        synced: bool,
    },
    /// The Project is visible. `synced` is false if its durability is uncertain.
    PlannedProjectCreated {
        project_id: Uuid,
        synced: bool,
    },
    /// The reminder is visible. `synced` is false if its durability is uncertain.
    ReminderCreated {
        reminder_id: Uuid,
        synced: bool,
    },
    /// The block is visible. `synced` is false if its durability is uncertain.
    TimeBlockCreated {
        work_unit_id: Uuid,
        synced: bool,
    },
    Workflow(workflow::WorkflowResponse),
    Error {
        code: ErrorCode,
        message: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    Conflict,
    StaleRevision,
    InvalidRequest,
    UnsupportedVersion,
    NotFound,
    PermissionDenied,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimerStatus {
    pub state: TimerState,
    /// The active session, or the most recently stopped session when idle.
    pub session: Option<FocusSession>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FocusSession {
    pub id: Uuid,
    pub work_unit_id: Option<Uuid>,
    pub started_at: DateTime<Utc>,
    pub stopped_at: Option<DateTime<Utc>>,
    pub elapsed_seconds: u64,
}
