use std::path::Path;

use chrono::{DateTime, NaiveDate};
use chrono_tz::Tz;
use crossterm::event::KeyCode;
use mcbtodo_application::VaultCommands;
use mcbtodo_domain::{
    Id, Project, Task,
    recurrence::ReminderSchedule,
    time::{DateSpan, Deadline, Span, ZonedMoment},
};
use mcbtodo_platform::WorkspacePaths;
use mcbtodo_protocol::{ErrorCode, ProtocolError, Request, Response};
use mcbtodo_vault::{Revision, Vault, WriteError};

use super::{Action, Overlay, State, View};

#[derive(Clone, Copy)]
pub(super) enum Kind {
    Project,
    Task,
    Event,
    Deadline,
    Reminder,
}

impl Kind {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Project => "Project",
            Self::Task => "Task",
            Self::Event => "All-day event",
            Self::Deadline => "Deadline",
            Self::Reminder => "Reminder",
        }
    }
}

pub(super) struct Form {
    pub kind: Kind,
    pub fields: Vec<Field>,
    pub selected: usize,
    task_project: Option<(Id, Revision)>,
    reminder: Option<(Tz, Option<Id>)>,
}

pub(super) struct Field {
    pub label: &'static str,
    pub value: String,
    pub default_date: Option<NaiveDate>,
}

impl Field {
    fn new(label: &'static str, default_date: Option<NaiveDate>) -> Self {
        Self {
            label,
            value: String::new(),
            default_date,
        }
    }

    fn date(&self) -> Result<NaiveDate, String> {
        if self.value.is_empty()
            && let Some(date) = self.default_date
        {
            return Ok(date);
        }
        crate::parse_day(&self.value).map_err(|error| format!("{} · {error}", self.label))
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Schedule {
    Project(DateSpan),
    Event(DateSpan),
    Deadline(NaiveDate),
    Reminder {
        at: ZonedMoment,
        target_id: Option<Id>,
    },
    Task {
        project_id: Id,
        expected_revision: Revision,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Submission {
    pub(super) title: String,
    pub(super) schedule: Schedule,
}

impl Form {
    pub(super) fn new(
        kind: Kind,
        day: NaiveDate,
        (start, through): (NaiveDate, NaiveDate),
    ) -> Self {
        let mut fields = vec![Field::new("Title", None)];
        match kind {
            Kind::Task | Kind::Reminder => {
                unreachable!("special forms have dedicated constructors")
            }
            Kind::Project | Kind::Event => {
                fields.push(Field::new("Start", Some(start)));
                fields.push(Field::new("Through", Some(through)));
            }
            Kind::Deadline => fields.push(Field::new("Due", Some(day))),
        }
        Self {
            kind,
            fields,
            selected: 0,
            task_project: None,
            reminder: None,
        }
    }

    pub(super) fn reminder(timezone: Tz, target_id: Option<Id>) -> Self {
        Self {
            kind: Kind::Reminder,
            fields: vec![
                Field::new("Title", None),
                Field::new("When (RFC3339)", None),
            ],
            selected: 0,
            task_project: None,
            reminder: Some((timezone, target_id)),
        }
    }

    pub(super) fn task(project_id: Id, expected_revision: Revision) -> Self {
        Self {
            kind: Kind::Task,
            fields: vec![Field::new("Title", None)],
            selected: 0,
            task_project: Some((project_id, expected_revision)),
            reminder: None,
        }
    }

    pub(super) fn handle(&mut self, code: KeyCode, message: &mut String) -> Option<Action> {
        let count = self.fields.len();
        let input = &mut self.fields[self.selected];
        match code {
            KeyCode::Esc => return None,
            KeyCode::Tab => self.selected = (self.selected + 1) % count,
            KeyCode::BackTab => self.selected = (self.selected + count - 1) % count,
            KeyCode::Enter if self.selected + 1 < count => self.selected += 1,
            KeyCode::Enter => match self.submission() {
                Ok(submission) => {
                    return Some(match submission.schedule {
                        Schedule::Task { .. } => Action::CreateTask(submission),
                        Schedule::Reminder { .. } => Action::CreateReminder(submission),
                        _ => Action::CreateCalendar(submission),
                    });
                }
                Err((field, error)) => {
                    self.selected = field;
                    *message = error;
                    return Some(Action::Continue);
                }
            },
            KeyCode::Backspace => {
                input.value.pop();
            }
            KeyCode::Delete => input.value.clear(),
            KeyCode::Char(ch) if !ch.is_control() => {
                input.value.push(ch);
            }
            _ => return Some(Action::Continue),
        }
        message.clear();
        Some(Action::Continue)
    }

    fn submission(&self) -> Result<Submission, (usize, String)> {
        let title = self.fields[0].value.trim();
        if title.is_empty() {
            return Err((0, "Enter a title".into()));
        }
        if matches!(self.kind, Kind::Task) {
            let (project_id, expected_revision) = self
                .task_project
                .as_ref()
                .expect("task form has a destination Project");
            return Ok(Submission {
                title: title.to_owned(),
                schedule: Schedule::Task {
                    project_id: *project_id,
                    expected_revision: expected_revision.clone(),
                },
            });
        }
        if matches!(self.kind, Kind::Reminder) {
            let value = DateTime::parse_from_rfc3339(&self.fields[1].value)
                .map_err(|error| (1, format!("When · expected RFC3339 timestamp · {error}")))?;
            let (timezone, target_id) = self.reminder.expect("reminder form has timezone");
            let at = ZonedMoment::new(value, timezone)
                .map_err(|error| (1, format!("When · {error}")))?;
            return Ok(Submission {
                title: title.to_owned(),
                schedule: Schedule::Reminder { at, target_id },
            });
        }
        let start = self.fields[1].date().map_err(|error| (1, error))?;
        let schedule = match self.kind {
            Kind::Task | Kind::Reminder => unreachable!("special submissions are created above"),
            Kind::Deadline => Schedule::Deadline(start),
            Kind::Project | Kind::Event => {
                let through = self.fields[2].date().map_err(|error| (2, error))?;
                let span = DateSpan::new(start, through).map_err(|error| (2, error.to_string()))?;
                match self.kind {
                    Kind::Project => Schedule::Project(span),
                    _ => Schedule::Event(span),
                }
            }
        };
        Ok(Submission {
            title: title.to_owned(),
            schedule,
        })
    }
}

impl State {
    pub(super) fn create_calendar(
        &mut self,
        root: &Path,
        vault: &mut Vault,
        paths: &WorkspacePaths,
        request: Submission,
    ) {
        let commands = VaultCommands::new(root, vault.timezone());
        let id = Id::new_v4();
        let title = request.title;
        let body = String::new();
        let (kind, day, saved) = match request.schedule {
            Schedule::Project(span) => (
                Kind::Project,
                span.start(),
                local_publication(
                    "Project",
                    commands.create_project(
                        Project {
                            id,
                            status: Default::default(),
                            active_period: Some(Span::DateSpan(span)),
                            deadline: None,
                            tags: Vec::new(),
                        },
                        None,
                        title,
                        body,
                    ),
                ),
            ),
            Schedule::Event(span) => {
                let label = Kind::Event.label();
                let saved = match is_daemon_vault(paths, root) {
                    Ok(true) => create_via_daemon(
                        paths,
                        Request::CreateAllDayEvent {
                            event_id: id,
                            title,
                            start: span.start(),
                            through: span.through(),
                        },
                        |response| match response {
                            Response::AllDayEventCreated { event_id, synced } if event_id == id => {
                                Some(synced)
                            }
                            _ => None,
                        },
                    )
                    .map(|synced| publication_message(label, id, synced)),
                    Ok(false) => {
                        local_publication(label, commands.create_all_day_event(id, span, title))
                    }
                    Err(error) => Err(CreationError::Rejected(format!("{error:#}"))),
                };
                (Kind::Event, span.start(), saved)
            }
            Schedule::Deadline(day) => (
                Kind::Deadline,
                day,
                local_publication(
                    "Deadline",
                    commands.create_project(
                        Project {
                            id,
                            status: Default::default(),
                            active_period: None,
                            deadline: Some(Deadline::Date { value: day }),
                            tags: Vec::new(),
                        },
                        None,
                        title,
                        body,
                    ),
                ),
            ),
            Schedule::Reminder { .. } => unreachable!("reminders use create_reminder"),
            Schedule::Task { .. } => unreachable!("task submissions use create_task"),
        };
        let label = kind.label();
        let saved = match saved {
            Ok(message) => message,
            Err(CreationError::Unknown) => {
                self.overlay = Overlay::None;
                self.reload(root, vault);
                self.message = format!("{label} outcome unknown · Check UUID {id} before retrying");
                return;
            }
            Err(CreationError::Rejected(message)) => {
                self.message = format!("{label} failed · {message}");
                return;
            }
            Err(CreationError::StaleRevision) => {
                self.message = format!("{label} failed · Vault changed");
                return;
            }
        };
        // Close the form after publication, including when the reload fails.
        self.overlay = Overlay::None;
        self.reload(root, vault);
        if self.message == "Vault reloaded" {
            self.anchor = None;
            self.set_day(day);
            self.switch(View::Day, vault);
            self.sync_row(vault, Some(super::FocusIdentity::Document(id)));
            self.document_id = Some(id);
            self.message = saved;
        } else {
            self.message = format!("{saved} · {}", self.message);
        }
    }

    pub(super) fn create_reminder(
        &mut self,
        root: &Path,
        vault: &mut Vault,
        paths: &WorkspacePaths,
        request: Submission,
    ) {
        let Schedule::Reminder { at, target_id } = request.schedule else {
            unreachable!("create_reminder receives a reminder submission");
        };
        let is_default_vault = paths.is_default_vault(root);
        if !matches!(is_default_vault, Ok(true)) {
            self.message = match is_default_vault {
                Ok(false) => {
                    "Background reminders are only supported for the default Vault".to_owned()
                }
                Err(error) => format!("Reminder failed · {error:#}"),
                Ok(true) => unreachable!(),
            };
            return;
        }
        let id = Id::new_v4();
        let saved = VaultCommands::new(root, vault.timezone()).create_reminder(
            id,
            target_id,
            ReminderSchedule::Once { at },
            request.title,
            String::new(),
        );
        let saved = match saved {
            Ok(_) => "Reminder saved".to_owned(),
            Err(WriteError::PublishedButNotSynced { source, .. }) => {
                format!("Reminder published · directory sync failed: {source}")
            }
            Err(error) => {
                self.message = format!("Reminder failed · {error:#}");
                return;
            }
        };
        self.overlay = Overlay::None;
        self.reload(root, vault);
        self.message = if self.message == "Vault reloaded" {
            saved
        } else {
            format!("{saved} · {}", self.message)
        };
    }

    pub(super) fn create_task(
        &mut self,
        root: &Path,
        vault: &mut Vault,
        paths: &WorkspacePaths,
        request: Submission,
    ) {
        let Schedule::Task {
            project_id,
            expected_revision,
        } = request.schedule
        else {
            unreachable!("create_task receives a task submission");
        };
        let id = Id::new_v4();
        let use_daemon = match is_daemon_vault(paths, root) {
            Ok(value) => value,
            Err(error) => {
                self.message = format!("Task capture failed · {error:#}");
                return;
            }
        };
        let saved = if use_daemon {
            create_via_daemon(
                paths,
                Request::CaptureTask {
                    task_id: id,
                    project_id,
                    project_revision: expected_revision.digest(),
                    title: request.title,
                },
                |response| match response {
                    Response::TaskCaptured { task_id, synced } if task_id == id => Some(synced),
                    _ => None,
                },
            )
            .map(|synced| publication_message("Task", id, synced))
        } else {
            match VaultCommands::new(root, vault.timezone()).capture_task(
                Task::new(id, project_id),
                request.title,
                String::new(),
                expected_revision,
            ) {
                Ok(_) => Ok("Task saved".to_owned()),
                Err(WriteError::StaleRevision { .. } | WriteError::ProjectNotFound { .. }) => {
                    Err(CreationError::StaleRevision)
                }
                Err(WriteError::PublishedButNotSynced { source, .. }) => {
                    Ok(format!("Task published · directory sync failed: {source}"))
                }
                Err(error) => Err(CreationError::Rejected(error.to_string())),
            }
        };
        let saved = match saved {
            Ok(message) => message,
            Err(CreationError::StaleRevision) => {
                self.message =
                    "Project changed · Esc, reload the Vault, then reopen Task capture".into();
                return;
            }
            Err(CreationError::Rejected(message)) => {
                self.message = format!("Task capture failed · {message}");
                return;
            }
            Err(CreationError::Unknown) => {
                self.overlay = Overlay::None;
                self.reload(root, vault);
                self.message = format!("Task outcome unknown · Check UUID {id} before retrying");
                return;
            }
        };
        self.overlay = Overlay::None;
        self.reload(root, vault);
        if self.message == "Vault reloaded" {
            self.switch(View::Outline, vault);
            self.sync_row(vault, Some(super::FocusIdentity::Document(id)));
            self.document_id = Some(id);
            self.message = saved;
        } else {
            self.message = format!("{saved} · {}", self.message);
        }
    }
}

enum CreationError {
    StaleRevision,
    Rejected(String),
    Unknown,
}

fn is_daemon_vault(paths: &WorkspacePaths, root: &Path) -> anyhow::Result<bool> {
    if cfg!(target_os = "linux") {
        paths.is_default_vault(root)
    } else {
        Ok(false)
    }
}

fn publication_message(label: &str, id: Id, synced: bool) -> String {
    if synced {
        format!("{label} saved")
    } else {
        format!("{label} {id} published · directory sync could not be confirmed")
    }
}

fn local_publication(
    label: &str,
    result: Result<mcbtodo_vault::PersistedDocument, WriteError>,
) -> Result<String, CreationError> {
    match result {
        Ok(_) => Ok(format!("{label} saved")),
        Err(WriteError::PublishedButNotSynced { source, .. }) => Ok(format!(
            "{label} published · directory sync failed: {source}"
        )),
        Err(error) => Err(CreationError::Rejected(format!("{error:#}"))),
    }
}

fn create_via_daemon(
    paths: &WorkspacePaths,
    request: Request,
    confirmation: impl FnOnce(Response) -> Option<bool>,
) -> Result<bool, CreationError> {
    match mcbtodo_protocol::request(paths.socket_path(), request) {
        Ok(Response::Error {
            code: ErrorCode::StaleRevision,
            ..
        }) => Err(CreationError::StaleRevision),
        Ok(Response::Error { message, .. }) => Err(CreationError::Rejected(message)),
        Ok(response) => confirmation(response).ok_or(CreationError::Unknown),
        Err(ProtocolError::Io(error))
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            Err(CreationError::Rejected(
                "Start mcbtodo-daemon and try again".into(),
            ))
        }
        Err(_) => Err(CreationError::Unknown),
    }
}
