use chrono::NaiveDate;
use chrono_tz::Tz;
use crossterm::event::KeyCode;
use mcbtodo_domain::{Id, time::Deadline};
use mcbtodo_vault::Revision;

use super::Action;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum EditRevision {
    Local(Revision),
    Daemon(String),
}

pub(super) const STALE: &str = "Document changed · press r to refresh the revision";

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
    pub(super) fn new(
        id: Id,
        revision: EditRevision,
        deadline: Option<Deadline>,
        timezone: Tz,
    ) -> Self {
        let original_day = deadline.and_then(|deadline| deadline.local_date(timezone));
        let value = original_day.map_or_else(String::new, |date| date.to_string());
        Self {
            id,
            value,
            cleared: deadline.is_none(),
            original: deadline,
            original_day,
            revision,
            needs_refresh: false,
        }
    }

    pub(super) fn refresh(
        &mut self,
        revision: EditRevision,
        deadline: Option<Deadline>,
        timezone: Tz,
    ) {
        self.revision = revision;
        self.original = deadline;
        self.original_day = deadline.and_then(|deadline| deadline.local_date(timezone));
        self.needs_refresh = false;
    }

    pub(super) fn handle(&mut self, code: KeyCode, message: &mut String) -> Option<Action> {
        match code {
            KeyCode::Esc => return None,
            KeyCode::Enter if self.needs_refresh => {
                *message = STALE.into();
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
                return Some(Action::RefreshDeadline);
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
