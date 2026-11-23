use anyhow::{Context, Result, anyhow, bail, ensure};
use chrono::{DateTime, FixedOffset, LocalResult, NaiveDate, NaiveTime, Offset, TimeZone};
use chrono_tz::Tz;
use crossterm::event::KeyCode;
use mcbtodo_application::{TargetAction, project_action_targets, project_outline};
use mcbtodo_domain::{Id, time::DateTimeSpan};
use mcbtodo_vault::{Revision, Vault};

use super::{Action, move_row};

pub(super) struct Form {
    pub day: NaiveDate,
    pub query: String,
    pub row: usize,
    pub unit: Option<Id>,
    pub start: String,
    pub end: String,
    pub field: Field,
    revision: Option<Revision>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Field {
    Start,
    End,
}

pub(super) struct Choice {
    pub id: Id,
    pub title: String,
    pub task: String,
    pub project: String,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Submission {
    pub unit: Id,
    pub when: DateTimeSpan,
    pub(super) revision: Revision,
}

impl Form {
    pub fn new(day: NaiveDate) -> Self {
        Self {
            day,
            query: String::new(),
            row: 0,
            unit: None,
            start: String::new(),
            end: String::new(),
            field: Field::Start,
            revision: None,
        }
    }

    pub fn choices(&self, vault: &Vault) -> Vec<Choice> {
        let query = self.query.to_lowercase();
        let eligible = project_action_targets(vault, TargetAction::CreateTimeBlock)
            .into_iter()
            .map(|target| target.id)
            .collect::<std::collections::BTreeSet<_>>();
        let (mut project, mut task) = (String::new(), String::new());
        let mut choices = Vec::new();
        for row in project_outline(vault) {
            match row.depth {
                0 => project = row.title,
                1 => task = row.title,
                2 if eligible.contains(&row.id)
                    && [row.title.as_str(), task.as_str(), project.as_str()]
                        .iter()
                        .any(|title| title.to_lowercase().contains(&query)) =>
                {
                    choices.push(Choice {
                        id: row.id,
                        title: row.title,
                        task: task.clone(),
                        project: project.clone(),
                    });
                }
                _ => {}
            }
        }
        choices
    }

    // None closes the form and returns to the calendar.
    pub fn handle(&mut self, code: KeyCode, vault: &Vault, message: &mut String) -> Option<Action> {
        if self.unit.is_none() {
            match code {
                KeyCode::Esc => return None,
                KeyCode::Enter => {
                    if let Some(choice) = self.choices(vault).get(self.row)
                        && let Some(revision) = vault.revision(choice.id)
                    {
                        self.unit = Some(choice.id);
                        self.revision = Some(revision.clone());
                        self.field = Field::Start;
                    }
                }
                KeyCode::Up => self.row = self.row.saturating_sub(1),
                KeyCode::Down => self.row = move_row(self.row, true, self.choices(vault).len()),
                KeyCode::Backspace => {
                    self.query.pop();
                    self.row = 0;
                }
                KeyCode::Char(ch) if !ch.is_control() => {
                    self.query.push(ch);
                    self.row = 0;
                }
                _ => {}
            }
            return Some(Action::Continue);
        }

        match code {
            KeyCode::Esc => {
                self.unit = None;
                self.revision = None;
            }
            KeyCode::Tab | KeyCode::BackTab => {
                self.field = match self.field {
                    Field::Start => Field::End,
                    Field::End => Field::Start,
                };
            }
            KeyCode::Enter => match self.request(vault) {
                Ok(Some(submission)) => return Some(Action::CreateTimeBlock(submission)),
                Ok(None) => {}
                Err(error) => {
                    *message = format!("{error:#}");
                    return Some(Action::Continue);
                }
            },
            KeyCode::Backspace => {
                self.input().pop();
            }
            KeyCode::Char(ch) if ch.is_ascii_digit() || matches!(ch, ':' | '+' | '-' | ' ') => {
                let input = self.input();
                if input.len() < 23 {
                    input.push(ch);
                }
            }
            _ => return Some(Action::Continue),
        }
        message.clear();
        Some(Action::Continue)
    }

    fn input(&mut self) -> &mut String {
        match self.field {
            Field::Start => &mut self.start,
            Field::End => &mut self.end,
        }
    }

    fn request(&mut self, vault: &Vault) -> Result<Option<Submission>> {
        let timezone = vault.timezone();
        let start = resolve_time(self.day, &self.start, false, timezone).inspect_err(|_| {
            self.field = Field::Start;
        })?;
        if self.field == Field::Start {
            self.field = Field::End;
            return Ok(None);
        }
        let end = resolve_time(self.day, &self.end, true, timezone)?;
        let when = DateTimeSpan::new(start, end, timezone)?;
        let unit = self.unit.context("Select a WorkUnit")?;
        let revision = self
            .revision
            .as_ref()
            .context("WorkUnit revision unavailable · Esc to cancel, r to reload")?
            .clone();
        Ok(Some(Submission {
            unit,
            when,
            revision,
        }))
    }
}

fn resolve_time(
    day: NaiveDate,
    input: &str,
    is_end: bool,
    timezone: Tz,
) -> Result<DateTime<FixedOffset>> {
    let input = input.trim();
    let (day, input) = match input.split_once(' ') {
        Some((date, rest)) if is_end && date.len() == 10 => (
            mcbtodo_client::parse_day(date).map_err(anyhow::Error::msg)?,
            rest,
        ),
        _ => (day, input),
    };
    let (clock, offset) = match input.split_once(' ') {
        Some((clock, offset)) => (
            clock,
            Some(offset.parse::<FixedOffset>().context("Use offset ±HH:MM")?),
        ),
        None => (input, None),
    };
    let local = if is_end && clock == "24:00" {
        day.succ_opt()
            .context("End date out of range")?
            .and_time(NaiveTime::MIN)
    } else {
        ensure!(clock.len() == 5, "Enter a time as HH:MM");
        day.and_time(NaiveTime::parse_from_str(clock, "%H:%M").context("Enter a time as HH:MM")?)
    };
    let matches_offset =
        |instant: &DateTime<Tz>| offset.is_none_or(|offset| instant.offset().fix() == offset);
    let chosen =
        match timezone.from_local_datetime(&local) {
            LocalResult::Single(instant) if matches_offset(&instant) => instant,
            LocalResult::Ambiguous(first, second) if offset.is_none() => bail!(
                "{clock} occurs twice · append {} or {}",
                first.offset().fix(),
                second.offset().fix()
            ),
            LocalResult::Ambiguous(first, second) => [first, second]
                .into_iter()
                .find(matches_offset)
                .ok_or_else(|| anyhow!("Offset does not match {clock} in {timezone}"))?,
            LocalResult::Single(_) => bail!("Offset does not match {clock} in {timezone}"),
            LocalResult::None => bail!("{clock} does not exist in {timezone} on {}", local.date()),
        };
    Ok(chosen.fixed_offset())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dst_gap_and_fold_require_a_real_unambiguous_instant() {
        let zone = chrono_tz::America::New_York;
        let gap = "2026-03-08".parse().unwrap();
        let fold = "2026-11-01".parse().unwrap();
        let missing = resolve_time(gap, "02:30", false, zone).unwrap_err();
        assert!(missing.to_string().contains("does not exist"));
        let ambiguous = resolve_time(fold, "01:30", false, zone).unwrap_err();
        assert!(ambiguous.to_string().contains("occurs twice"));
        let start = resolve_time(fold, "01:30 -04:00", false, zone).unwrap();
        let end = resolve_time(fold, "01:30 -05:00", true, zone).unwrap();
        assert_eq!((end - start).num_hours(), 1);
        assert!(resolve_time(fold, "01:30 -06:00", false, zone).is_err());
        assert!(DateTimeSpan::new(start, end, zone).is_ok());
    }

    #[test]
    fn midnight_end_is_the_start_of_the_next_day() {
        let day = "2026-11-20".parse().unwrap();
        let zone = chrono_tz::Asia::Shanghai;
        let end = resolve_time(day, "24:00", true, zone).unwrap();
        assert_eq!(end.to_rfc3339(), "2026-11-21T00:00:00+08:00");
        assert!(resolve_time(day, "24:00", false, zone).is_err());
    }
}
