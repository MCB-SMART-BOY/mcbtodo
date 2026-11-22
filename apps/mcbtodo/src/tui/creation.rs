use std::path::Path;

use chrono::{DateTime, NaiveDate};
use chrono_tz::Tz;
use crossterm::event::KeyCode;
use mcbtodo_application::VaultCommands;
use mcbtodo_domain::{
    Availability, CalendarEvent, Id, Priority, Project, Task,
    recurrence::ReminderSchedule,
    time::{DateSpan, Deadline, Span, ZonedMoment},
};
use mcbtodo_platform::WorkspacePaths;
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
    pub(super) fn create_calendar(&mut self, root: &Path, vault: &mut Vault, request: Submission) {
        let commands = VaultCommands::new(root, vault.timezone());
        let id = Id::new_v4();
        let title = request.title;
        let body = String::new();
        let (kind, day, saved) = match request.schedule {
            Schedule::Project(span) => (
                Kind::Project,
                span.start(),
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
            Schedule::Event(span) => (
                Kind::Event,
                span.start(),
                commands.create_event(
                    CalendarEvent {
                        id,
                        when: Span::DateSpan(span),
                        recurrence: None,
                        availability: Availability::Busy,
                        tags: Vec::new(),
                    },
                    title,
                    body,
                ),
            ),
            Schedule::Deadline(day) => (
                Kind::Deadline,
                day,
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
            Schedule::Reminder { .. } => unreachable!("reminders use create_reminder"),
            Schedule::Task { .. } => unreachable!("task submissions use create_task"),
        };
        let label = kind.label();
        let saved = match saved {
            Ok(_) => format!("{label} saved"),
            Err(WriteError::PublishedButNotSynced { source, .. }) => {
                format!("{label} published · directory sync failed: {source}")
            }
            Err(error) => {
                self.message = format!("{label} failed · {error:#}");
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

    pub(super) fn create_task(&mut self, root: &Path, vault: &mut Vault, request: Submission) {
        let Schedule::Task {
            project_id,
            expected_revision,
        } = request.schedule
        else {
            unreachable!("create_task receives a task submission");
        };
        let id = Id::new_v4();
        let title = request.title;
        let saved = VaultCommands::new(root, vault.timezone()).capture_task(
            Task {
                id,
                project_id,
                status: Default::default(),
                priority: Priority::default(),
                active_period: None,
                deadline: None,
                recurrence: None,
                occurrence: None,
                tags: Vec::new(),
            },
            title,
            String::new(),
            expected_revision,
        );
        let saved = match saved {
            Ok(_) => "Task saved".to_owned(),
            Err(WriteError::StaleRevision { .. }) => {
                self.message =
                    "Project changed · Esc, reload the Vault, then reopen Task capture".into();
                return;
            }
            Err(WriteError::PublishedButNotSynced { source, .. }) => {
                format!("Task published · directory sync failed: {source}")
            }
            Err(error) => {
                self.message = format!("Task capture failed · {error:#}");
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
