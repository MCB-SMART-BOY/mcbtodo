use anyhow::{Context, Result};
use chrono::NaiveDate;
use crossterm::event::KeyCode;
use mcbtodo_application::deadline_edit_context;
use mcbtodo_domain::{Id, time::Deadline};
use mcbtodo_vault::{Revision, Vault};

use super::{Action, daemon_views::DaemonViews};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum EditRevision {
    Local(Revision),
    Daemon(String),
}

pub(super) const DAEMON_STALE: &str = "Document changed · press r to refresh the revision";

pub(super) struct Form {
    pub id: Id,
    pub value: String,
    pub cleared: bool,
    original: Option<Deadline>,
    original_day: Option<NaiveDate>,
    revision: EditRevision,
    pub(super) needs_refresh: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Submission {
    pub(super) id: Id,
    pub(super) deadline: Option<Deadline>,
    pub(super) revision: EditRevision,
}

impl Form {
    pub(super) fn open(id: Id, vault: &Vault, service: Option<&DaemonViews>) -> Result<Self> {
        let (revision, deadline) = match service {
            Some(service) => {
                let (revision, deadline) = service.deadline_context(id)?;
                (EditRevision::Daemon(revision), deadline)
            }
            None => {
                let context = deadline_edit_context(vault, id)
                    .context("Selection does not support deadlines")?;
                (EditRevision::Local(context.revision), context.deadline)
            }
        };
        let original_day = deadline.and_then(|deadline| deadline.local_date(vault.timezone()));
        let value = original_day.map_or_else(String::new, |date| date.to_string());
        Ok(Self {
            id,
            value,
            cleared: deadline.is_none(),
            original: deadline,
            original_day,
            revision,
            needs_refresh: false,
        })
    }

    pub(super) fn handle(
        &mut self,
        code: KeyCode,
        vault: &Vault,
        service: Option<&DaemonViews>,
        message: &mut String,
    ) -> Option<Action> {
        match code {
            KeyCode::Esc => return None,
            KeyCode::Enter if self.needs_refresh && service.is_some() => {
                *message = DAEMON_STALE.into();
                return Some(Action::Continue);
            }
            KeyCode::Enter => {
                let deadline = if self.cleared {
                    None
                } else {
                    match crate::parse_day(&self.value) {
                        Ok(value) if self.original_day == Some(value) => self.original,
                        Ok(value) => Some(Deadline::Date { value }),
                        Err(error) => {
                            *message = error;
                            return Some(Action::Continue);
                        }
                    }
                };
                return Some(Action::SetDeadline(Submission {
                    id: self.id,
                    deadline,
                    revision: self.revision.clone(),
                }));
            }
            KeyCode::Delete => {
                self.value.clear();
                self.cleared = true;
            }
            KeyCode::Backspace => {
                self.value.pop();
                self.cleared = false;
            }
            KeyCode::Char('r') if self.needs_refresh => {
                if let Some(service) = service {
                    match service.deadline_context(self.id) {
                        Ok((revision, deadline)) => {
                            self.revision = EditRevision::Daemon(revision);
                            self.original = deadline;
                            self.original_day =
                                deadline.and_then(|deadline| deadline.local_date(vault.timezone()));
                        }
                        Err(error) => {
                            *message = format!("Deadline refresh failed · {error:#}");
                            return Some(Action::Continue);
                        }
                    }
                } else {
                    let Some(context) = deadline_edit_context(vault, self.id) else {
                        *message = "Selected document is no longer available".into();
                        return Some(Action::Continue);
                    };
                    self.revision = EditRevision::Local(context.revision);
                }
                self.needs_refresh = false;
                *message = "Revision refreshed; review the date before saving".into();
                return Some(Action::Continue);
            }
            KeyCode::Char(ch) if ch.is_ascii_digit() || ch == '-' => {
                if self.value.len() < 10 {
                    self.value.push(ch);
                    self.cleared = false;
                }
            }
            _ => return Some(Action::Continue),
        }
        if !self.needs_refresh {
            message.clear();
        }
        Some(Action::Continue)
    }
}
