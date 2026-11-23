use anyhow::{Context, Result, anyhow, bail, ensure};
use chrono::{DateTime, FixedOffset, LocalResult, NaiveDate, NaiveTime, Offset, TimeZone};
use chrono_tz::Tz;
use crossterm::event::KeyCode;
use mcbtodo_domain::{Id, OutlineItem, time::DateTimeSpan};

use super::{Action, move_row};

pub(super) struct Form {
    pub day: NaiveDate,
    pub query: String,
    pub row: usize,
    choices: Vec<Choice>,
    selected: Option<usize>,
    pub timezone: Tz,
    pub start: String,
    pub end: String,
    pub field: Field,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Field {
    Start,
    End,
}

pub(super) struct Choice {
    id: Id,
    revision: String,
    pub title: String,
    pub task: String,
    pub project: String,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Submission {
    pub unit: Id,
    pub when: DateTimeSpan,
    pub(super) revision: String,
}

impl Form {
    pub fn new(day: NaiveDate, timezone: Tz, outline: &[OutlineItem]) -> Self {
        let (mut project, mut task) = ("", "");
        let mut choices = Vec::new();
        for row in outline {
            match row.depth {
                0 => project = &row.title,
                1 => task = &row.title,
                // Every WorkUnit is eligible for a time block.
                2 => choices.push(Choice {
                    id: row.id,
                    revision: row.revision.clone(),
                    title: row.title.clone(),
                    task: task.to_owned(),
                    project: project.to_owned(),
                }),
                _ => {}
            }
        }
        Self {
            day,
            timezone,
            query: String::new(),
            row: 0,
            choices,
            selected: None,
            start: String::new(),
            end: String::new(),
            field: Field::Start,
        }
    }

    pub fn selected(&self) -> Option<&Choice> {
        self.selected.map(|index| &self.choices[index])
    }

    pub fn matches(&self) -> impl Iterator<Item = (usize, &Choice)> {
        let query = self.query.to_lowercase();
        self.choices.iter().enumerate().filter(move |(_, choice)| {
            [&choice.title, &choice.task, &choice.project]
                .into_iter()
                .any(|title| title.to_lowercase().contains(&query))
        })
    }

    // None closes the form and returns to the calendar.
    pub fn handle(&mut self, code: KeyCode, message: &mut String) -> Option<Action> {
        if self.selected.is_none() {
            match code {
                KeyCode::Esc => return None,
                KeyCode::Enter => {
                    let selected = self.matches().nth(self.row).map(|(index, _)| index);
                    if selected.is_some() {
                        self.selected = selected;
                        self.field = Field::Start;
                    }
                }
                KeyCode::Up => self.row = self.row.saturating_sub(1),
                KeyCode::Down => self.row = move_row(self.row, true, self.matches().count()),
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
            KeyCode::Esc => self.selected = None,
            KeyCode::Tab | KeyCode::BackTab => {
                self.field = match self.field {
                    Field::Start => Field::End,
                    Field::End => Field::Start,
                };
            }
            KeyCode::Enter => match self.request() {
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

    fn request(&mut self) -> Result<Option<Submission>> {
        let timezone = self.timezone;
        let start = resolve_time(self.day, &self.start, false, timezone).inspect_err(|_| {
            self.field = Field::Start;
        })?;
        if self.field == Field::Start {
            self.field = Field::End;
            return Ok(None);
        }
        let end = resolve_time(self.day, &self.end, true, timezone)?;
        let when = DateTimeSpan::new(start, end, timezone)?;
        let choice = self.selected().context("Select a WorkUnit")?;
        Ok(Some(Submission {
            unit: choice.id,
            when,
            revision: choice.revision.clone(),
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
