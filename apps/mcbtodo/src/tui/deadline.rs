use chrono::NaiveDate;
use chrono_tz::Tz;
use crossterm::event::KeyCode;
use mcbtodo_application::deadline_edit_context;
use mcbtodo_domain::{Id, time::Deadline};
use mcbtodo_vault::{Revision, Vault};

use super::Action;

pub(super) struct Form {
    pub id: Id,
    pub value: String,
    pub cleared: bool,
    original: Option<Deadline>,
    original_day: Option<NaiveDate>,
    pub(super) revision: Revision,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Submission {
    pub(super) id: Id,
    pub(super) deadline: Option<Deadline>,
    pub(super) revision: Revision,
}

impl Form {
    pub fn new(id: Id, revision: Revision, deadline: Option<Deadline>, timezone: Tz) -> Self {
        let original_day = deadline.and_then(|deadline| deadline.local_date(timezone));
        let value = original_day.map_or_else(String::new, |date| date.to_string());
        Self {
            id,
            value,
            cleared: deadline.is_none(),
            original: deadline,
            original_day,
            revision,
        }
    }

    pub fn handle(&mut self, code: KeyCode, vault: &Vault, message: &mut String) -> Option<Action> {
        match code {
            KeyCode::Esc => return None,
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
            KeyCode::Char('r') if message.contains("changed") => {
                let Some(context) = deadline_edit_context(vault, self.id) else {
                    *message = "Selected document is no longer available".into();
                    return Some(Action::Continue);
                };
                self.revision = context.revision;
                *message = "Revision refreshed; review the date before saving".into();
            }
            KeyCode::Char(ch) if ch.is_ascii_digit() || ch == '-' => {
                if self.value.len() < 10 {
                    self.value.push(ch);
                    self.cleared = false;
                }
            }
            _ => return Some(Action::Continue),
        }
        if !message.contains("changed") {
            message.clear();
        }
        Some(Action::Continue)
    }
}
