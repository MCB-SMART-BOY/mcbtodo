//! IPC used by the CLI and terminal client. The protocol crate owns framing and peer checks.

use std::{
    collections::BTreeSet,
    io,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, FixedOffset};
use chrono_tz::Tz;
use mcbtodo_application::ProjectPlan;
use mcbtodo_domain::{
    CalendarItem, DependencyOperation, Id, InboxSummary, OutlineItem, WorkUnitDependencyPreview,
    WorkUnitGraphNode,
    time::{DateSpan, DateTimeSpan, Deadline},
};
use mcbtodo_platform::WorkspacePaths;
use mcbtodo_protocol::{
    ErrorCode, ProjectDates, ProtocolError, Request, Response, TimerStatus, VaultWatchStatus,
    request,
};

#[derive(Debug, PartialEq, Eq)]
pub enum TimerOperation {
    Status,
    Start { work_unit_id: Option<Id> },
    Pause,
    Stop,
}

pub fn timer(paths: &WorkspacePaths, operation: TimerOperation) -> Result<TimerStatus> {
    let request = match operation {
        TimerOperation::Status => Request::Status,
        TimerOperation::Start { work_unit_id } => Request::Start { work_unit_id },
        TimerOperation::Pause => Request::Pause,
        TimerOperation::Stop => Request::Stop,
    };
    match exchange(paths, request)? {
        Response::Status(status) => Ok(status),
        _ => bail!("unexpected response to timer request"),
    }
}

pub fn create_planned_project(
    paths: &WorkspacePaths,
    id: Id,
    title: String,
    plan: ProjectPlan,
) -> Result<bool, WriteFailure> {
    let dates = match plan {
        ProjectPlan::ActivePeriod(span) => ProjectDates::ActivePeriod {
            start: span.start(),
            through: span.through(),
        },
        ProjectPlan::Deadline(due) => ProjectDates::Deadline { due },
    };
    submit_write(
        paths,
        Request::CreatePlannedProject {
            project_id: id,
            title,
            dates,
        },
        |response| match response {
            Response::PlannedProjectCreated { project_id, synced } if project_id == id => {
                Some(synced)
            }
            _ => None,
        },
    )
}

pub fn create_all_day_event(
    paths: &WorkspacePaths,
    id: Id,
    title: String,
    span: DateSpan,
) -> Result<bool, WriteFailure> {
    submit_write(
        paths,
        Request::CreateAllDayEvent {
            event_id: id,
            title,
            start: span.start(),
            through: span.through(),
        },
        |response| match response {
            Response::AllDayEventCreated { event_id, synced } if event_id == id => Some(synced),
            _ => None,
        },
    )
}

pub fn create_reminder(
    paths: &WorkspacePaths,
    id: Id,
    target_id: Option<Id>,
    title: String,
    at: DateTime<FixedOffset>,
) -> Result<bool, WriteFailure> {
    submit_write(
        paths,
        Request::CreateReminder {
            reminder_id: id,
            target_id,
            title,
            at,
        },
        |response| match response {
            Response::ReminderCreated {
                reminder_id,
                synced,
            } if reminder_id == id => Some(synced),
            _ => None,
        },
    )
}

pub fn create_time_block(
    paths: &WorkspacePaths,
    id: Id,
    revision: String,
    when: DateTimeSpan,
) -> Result<bool, WriteFailure> {
    submit_write(
        paths,
        Request::CreateTimeBlock {
            work_unit_id: id,
            work_unit_revision: revision,
            start: when.start(),
            end: when.end(),
        },
        |response| match response {
            Response::TimeBlockCreated {
                work_unit_id,
                synced,
            } if work_unit_id == id => Some(synced),
            _ => None,
        },
    )
}

pub fn capture_task(
    paths: &WorkspacePaths,
    id: Id,
    project_id: Id,
    project_revision: String,
    title: String,
) -> Result<bool, WriteFailure> {
    submit_write(
        paths,
        Request::CaptureTask {
            task_id: id,
            project_id,
            project_revision,
            title,
        },
        |response| match response {
            Response::TaskCaptured { task_id, synced } if task_id == id => Some(synced),
            _ => None,
        },
    )
}

pub fn vault_watch_status(paths: &WorkspacePaths) -> Result<VaultWatchStatus> {
    match exchange(paths, Request::VaultWatch)? {
        Response::VaultWatch(status) => Ok(status),
        _ => bail!("unexpected response to Vault watch request"),
    }
}

pub fn calendar(
    paths: &WorkspacePaths,
    window: DateSpan,
    timezone: Tz,
) -> Result<Vec<CalendarItem>> {
    match exchange(
        paths,
        Request::Calendar {
            start: window.start(),
            through: window.through(),
            timezone: timezone.name().into(),
        },
    )? {
        Response::Calendar { items } => Ok(items),
        _ => bail!("unexpected response to calendar request"),
    }
}

/// Read the complete outline projection for a display timezone.
pub fn outline(paths: &WorkspacePaths, timezone: Tz) -> Result<Vec<OutlineItem>> {
    match exchange(
        paths,
        Request::Outline {
            timezone: timezone.name().into(),
        },
    )? {
        Response::Outline { items, more: false } => Ok(items),
        Response::Outline { more: true, .. } => bail!("daemon returned an incomplete outline"),
        _ => bail!("unexpected response to outline request"),
    }
}

/// The value and revision observed together when opening a deadline editor.
pub struct DeadlineContext {
    pub revision: String,
    pub deadline: Option<Deadline>,
}

pub fn deadline_context(paths: &WorkspacePaths, id: Id) -> Result<DeadlineContext> {
    match exchange(paths, Request::DeadlineContext { id })? {
        Response::DeadlineContext { revision, deadline } => {
            Ok(DeadlineContext { revision, deadline })
        }
        _ => bail!("unexpected response to deadline context request"),
    }
}

/// Set or clear the deadline using the revision the user reviewed.
/// On success, the returned flag indicates whether directory sync was confirmed.
pub fn set_deadline(
    paths: &WorkspacePaths,
    id: Id,
    revision: String,
    deadline: Option<Deadline>,
) -> Result<bool, WriteFailure> {
    submit_write(
        paths,
        Request::SetDeadline {
            id,
            revision,
            deadline,
        },
        |response| match response {
            Response::DeadlineSet { synced } => Some(synced),
            _ => None,
        },
    )
}

pub fn inbox(paths: &WorkspacePaths) -> Result<Vec<InboxSummary>> {
    match exchange(paths, Request::Inbox)? {
        Response::Inbox { items, more: false } => Ok(items),
        _ => bail!("unexpected response to Inbox request"),
    }
}

pub fn triage_inbox(
    paths: &WorkspacePaths,
    inbox_id: Id,
    project_id: Id,
    inbox_revision: String,
) -> Result<bool, WriteFailure> {
    submit_write(
        paths,
        Request::TriageInbox {
            inbox_id,
            project_id,
            inbox_revision,
        },
        |response| match response {
            Response::InboxTriaged { synced } => Some(synced),
            _ => None,
        },
    )
}

pub fn work_unit_graph(paths: &WorkspacePaths, timezone: Tz) -> Result<Vec<WorkUnitGraphNode>> {
    match exchange(
        paths,
        Request::WorkUnitGraph {
            timezone: timezone.name().into(),
        },
    )? {
        Response::WorkUnitGraph { items, more: false } => Ok(items),
        _ => bail!("unexpected response to WorkUnit graph request"),
    }
}

pub fn preview_work_unit_dependencies(
    paths: &WorkspacePaths,
    target: Id,
    sources: &BTreeSet<Id>,
    operation: DependencyOperation,
    timezone: Tz,
) -> Result<WorkUnitDependencyPreview> {
    match exchange(
        paths,
        Request::PreviewWorkUnitDependencyChange {
            target,
            sources: sources.clone(),
            operation,
            timezone: timezone.name().into(),
        },
    )? {
        Response::WorkUnitDependencyPreview(edit) => Ok(edit),
        _ => bail!("unexpected response to dependency preview"),
    }
}

pub fn set_work_unit_dependencies(
    paths: &WorkspacePaths,
    target: Id,
    prerequisites: Vec<Id>,
    revision: String,
) -> Result<bool, WriteFailure> {
    submit_write(
        paths,
        Request::SetWorkUnitDependencies {
            target,
            prerequisites,
            revision,
        },
        |response| match response {
            Response::WorkUnitDependenciesSet { synced } => Some(synced),
            _ => None,
        },
    )
}

pub fn document_source(paths: &WorkspacePaths, id: Id) -> Result<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};

        match exchange(paths, Request::DocumentSource { id })? {
            Response::DocumentSource { unix_path } => Ok(OsString::from_vec(unix_path).into()),
            _ => bail!("unexpected response to document source request"),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (paths, id);
        bail!("local daemon document lookup is unavailable on this platform")
    }
}

pub fn exchange(paths: &WorkspacePaths, operation: Request) -> Result<Response> {
    let socket = paths.socket_path();
    let exchange = request(&socket, operation);
    if let Err(ProtocolError::Io(error)) = &exchange
        && matches!(
            error.kind(),
            io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
        )
    {
        return exchange.context("Start mcbtodo-daemon and try again");
    }
    match exchange.with_context(|| format!("contacting mcbtodo-daemon at {}", socket.display()))? {
        Response::Error { code, message } => bail!("{code:?}: {message}"),
        response => Ok(response),
    }
}

pub enum WriteFailure {
    StaleRevision,
    Rejected(String),
    /// The daemon may have acted, but no recognized confirmation arrived.
    Unknown,
}

pub fn owns_vault(paths: &WorkspacePaths, root: &Path) -> Result<bool> {
    if cfg!(target_os = "linux") {
        paths.is_default_vault(root)
    } else {
        Ok(false)
    }
}

fn submit_write(
    paths: &WorkspacePaths,
    operation: Request,
    confirmation: impl FnOnce(Response) -> Option<bool>,
) -> Result<bool, WriteFailure> {
    match request(paths.socket_path(), operation) {
        Ok(Response::Error {
            code: ErrorCode::StaleRevision,
            ..
        }) => Err(WriteFailure::StaleRevision),
        Ok(Response::Error { message, .. }) => Err(WriteFailure::Rejected(message)),
        Ok(response) => confirmation(response).ok_or(WriteFailure::Unknown),
        Err(ProtocolError::Io(error))
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            Err(WriteFailure::Rejected(
                "Start mcbtodo-daemon and try again".into(),
            ))
        }
        Err(_) => Err(WriteFailure::Unknown),
    }
}
