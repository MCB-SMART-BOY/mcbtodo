use std::{
    collections::BTreeMap,
    io::{self, Write},
    path::Path,
};

use anyhow::{Context, Result, bail, ensure};
use chrono::{Datelike, Days, NaiveDate, Utc};
use chrono_tz::Tz;
use clap::Args;
use mcbtodo_application::{ProjectSummary, VaultQueries, WorkUnitSummary};
use mcbtodo_domain::Id;
use mcbtodo_platform::WorkspacePaths;
use mcbtodo_protocol::{Request, Response, TimerState, TimerStatus, focus::DailyFocusReport};
use serde::Serialize;

pub(crate) fn timer_label(status: &TimerStatus) -> String {
    let state = match status.state {
        TimerState::Idle => "idle",
        TimerState::Running => "running",
        TimerState::Paused => "paused",
        TimerState::NeedsReview => "needs review",
    };
    match &status.session {
        Some(session) => format!(
            "Timer {state} · {}s · {}",
            session.elapsed_seconds, session.id
        ),
        None => format!("Timer {state}"),
    }
}

// The daemon exposes one snapshot per day and scans the history for each request.
const MAX_REPORT_DAYS: i64 = 366;

#[derive(Args)]
pub(crate) struct ReportArgs {
    /// Report this local date, or use it as the anchor for --week (default: today).
    #[arg(long, value_parser = crate::parse_day, conflicts_with = "from")]
    date: Option<NaiveDate>,
    /// Report the local Monday–Sunday week containing the anchor date.
    #[arg(long, conflicts_with = "from")]
    week: bool,
    /// First local date of an inclusive range (at most 366 days).
    #[arg(long, value_parser = crate::parse_day, requires = "to")]
    from: Option<NaiveDate>,
    /// Last local date of an inclusive range.
    #[arg(long, value_parser = crate::parse_day, requires = "from")]
    to: Option<NaiveDate>,
    /// Include only sessions for WorkUnits in this project in the selected Vault.
    #[arg(long, value_name = "UUID")]
    project: Option<Id>,
    /// Write CSV to stdout instead of Markdown.
    #[arg(long, conflicts_with = "json")]
    csv: bool,
}

impl ReportArgs {
    fn dates(&self, today: NaiveDate) -> Result<(NaiveDate, NaiveDate)> {
        let (first, last) = match (self.from, self.to) {
            (Some(first), Some(last)) => (first, last),
            (None, None) => {
                let anchor = self.date.unwrap_or(today);
                if self.week {
                    let monday = anchor
                        .checked_sub_days(Days::new(anchor.weekday().num_days_from_monday().into()))
                        .context("week starts before the supported date range")?;
                    let sunday = monday
                        .checked_add_days(Days::new(6))
                        .context("week ends after the supported date range")?;
                    ensure!(
                        monday.year() >= 1 && sunday.year() <= 9999,
                        "week falls outside the supported years 0001–9999"
                    );
                    (monday, sunday)
                } else {
                    (anchor, anchor)
                }
            }
            _ => bail!("provide both --from and --to"),
        };
        ensure!(first <= last, "--from must be on or before --to");
        ensure!(
            (last - first).num_days() < MAX_REPORT_DAYS,
            "focus report range may include at most {MAX_REPORT_DAYS} days"
        );
        Ok((first, last))
    }
}

#[derive(Default, Serialize)]
struct Totals {
    elapsed_seconds: u64,
    session_count: u64,
    needs_review_count: u64,
}

impl Totals {
    fn add(&mut self, elapsed: u64, sessions: u64, needs_review: u64) -> Result<()> {
        self.elapsed_seconds = self
            .elapsed_seconds
            .checked_add(elapsed)
            .context("focus elapsed time overflow")?;
        self.session_count = self
            .session_count
            .checked_add(sessions)
            .context("focus session count overflow")?;
        self.needs_review_count = self
            .needs_review_count
            .checked_add(needs_review)
            .context("focus review count overflow")?;
        Ok(())
    }
}

#[derive(Serialize)]
struct UnitReport {
    work_unit_id: Option<Id>,
    project_id: Option<Id>,
    #[serde(flatten)]
    totals: Totals,
    estimated_seconds: Option<u64>,
    variance_seconds: Option<i128>,
}

#[derive(Serialize)]
struct Report {
    /// First local date (the sole date for a daily report).
    date: NaiveDate,
    end_date: NaiveDate,
    timezone: Tz,
    project_id: Option<Id>,
    total_elapsed_seconds: u64,
    session_count: u64,
    needs_review_count: u64,
    by_work_unit: Vec<UnitReport>,
}

pub(crate) fn run(
    paths: &WorkspacePaths,
    root: &Path,
    timezone: Tz,
    args: ReportArgs,
    json: bool,
) -> Result<()> {
    let today = Utc::now().with_timezone(&timezone).date_naive();
    let dates = args.dates(today)?;
    let queries = VaultQueries::new(root, timezone);
    let context = queries.focus_report_context(args.project)?;
    let report = collect(
        &context.projects,
        &context.work_units,
        timezone,
        dates,
        args.project,
        |request| crate::daemon_client::exchange(paths, request),
    )?;

    let mut output = io::stdout().lock();
    if json {
        serde_json::to_writer_pretty(&mut output, &report)?;
        writeln!(output)?;
    } else if args.csv {
        report.write_csv(&mut output)?;
    } else {
        report.write_markdown(&mut output)?;
    }
    Ok(())
}

fn collect(
    projects: &[ProjectSummary],
    work_units: &[WorkUnitSummary],
    timezone: Tz,
    (first, last): (NaiveDate, NaiveDate),
    project: Option<Id>,
    mut request: impl FnMut(Request) -> Result<Response>,
) -> Result<Report> {
    if let Some(id) = project {
        ensure!(
            projects.iter().any(|row| row.id == id),
            "no project with ID {id} in the vault"
        );
    }
    let zone = timezone.to_string();
    let work_units: BTreeMap<_, _> = work_units.iter().map(|unit| (unit.id, unit)).collect();
    let mut total = Totals::default();
    let mut units = BTreeMap::<Option<Id>, UnitReport>::new();
    for date in first.iter_days().take_while(|date| *date <= last) {
        let Response::FocusReport(DailyFocusReport {
            date: received_date,
            timezone: received_zone,
            total_elapsed_seconds,
            session_count,
            needs_review_count,
            by_work_unit,
        }) = request(Request::FocusReport {
            date,
            timezone: zone.clone(),
        })
        .with_context(|| format!("querying focus time for {date} ({timezone})"))?
        else {
            bail!("unexpected response to focus report request for {date}");
        };
        ensure!(
            received_date == date && received_zone == zone,
            "mismatched focus report response for {date} ({timezone})"
        );
        if project.is_none() {
            // The daemon sums before truncating to seconds, so its total can
            // exceed the sum of individually truncated WorkUnit groups.
            total.add(total_elapsed_seconds, session_count, needs_review_count)?;
        }
        for row in by_work_unit {
            let unit = row.work_unit_id.and_then(|id| work_units.get(&id).copied());
            let project_id = unit.map(|unit| unit.project_id);
            if project.is_some() {
                if project != project_id {
                    continue;
                }
                total.add(
                    row.elapsed_seconds,
                    row.session_count,
                    row.needs_review_count,
                )?;
            }
            units
                .entry(row.work_unit_id)
                .or_insert_with(|| UnitReport {
                    work_unit_id: row.work_unit_id,
                    project_id,
                    totals: Totals::default(),
                    estimated_seconds: unit
                        .and_then(|unit| unit.estimated_minutes)
                        .map(|minutes| u64::from(minutes) * 60),
                    variance_seconds: None,
                })
                .totals
                .add(
                    row.elapsed_seconds,
                    row.session_count,
                    row.needs_review_count,
                )?;
        }
    }
    let mut by_work_unit: Vec<_> = units.into_values().collect();
    for unit in &mut by_work_unit {
        unit.variance_seconds = unit
            .estimated_seconds
            .map(|estimate| i128::from(unit.totals.elapsed_seconds) - i128::from(estimate));
    }
    Ok(Report {
        date: first,
        end_date: last,
        timezone,
        project_id: project,
        total_elapsed_seconds: total.elapsed_seconds,
        session_count: total.session_count,
        needs_review_count: total.needs_review_count,
        by_work_unit,
    })
}

impl Report {
    fn write_markdown(&self, output: &mut impl Write) -> io::Result<()> {
        let date = if self.date == self.end_date {
            self.date.to_string()
        } else {
            format!("{} – {}", self.date, self.end_date)
        };
        writeln!(output, "# Focus · {date} ({})\n", self.timezone)?;
        if let Some(project) = self.project_id {
            writeln!(output, "Project `{project}`\n")?;
        }
        writeln!(
            output,
            "{} seconds across {} sessions ({} need review).\n",
            self.total_elapsed_seconds, self.session_count, self.needs_review_count
        )?;
        writeln!(
            output,
            "| Work unit | Actual (s) | Estimate (s) | Actual − estimate (s) | Sessions | Need review |"
        )?;
        writeln!(output, "| --- | ---: | ---: | ---: | ---: | ---: |")?;
        for row in &self.by_work_unit {
            let unit = row
                .work_unit_id
                .map_or_else(|| "Unassigned".into(), |id| format!("`{id}`"));
            let estimate = row
                .estimated_seconds
                .map_or_else(|| "—".into(), |value| value.to_string());
            let variance = row
                .variance_seconds
                .map_or_else(|| "—".into(), |value| format!("{value:+}"));
            writeln!(
                output,
                "| {unit} | {} | {estimate} | {variance} | {} | {} |",
                row.totals.elapsed_seconds, row.totals.session_count, row.totals.needs_review_count
            )?;
        }
        Ok(())
    }

    fn write_csv(&self, output: &mut impl Write) -> io::Result<()> {
        // Fields contain only IDs, calendar dates, a canonical IANA name, or
        // numbers. No user-authored text can introduce delimiters or formulas.
        write!(
            output,
            "kind,date,end_date,timezone,project_id,work_unit_id,elapsed_seconds,estimated_seconds,variance_seconds,session_count,needs_review_count\r\n"
        )?;
        let prefix = format!("{},{},{}", self.date, self.end_date, self.timezone);
        write!(
            output,
            "total,{prefix},{},,{},,,{},{}\r\n",
            csv_field(self.project_id),
            self.total_elapsed_seconds,
            self.session_count,
            self.needs_review_count
        )?;
        for row in &self.by_work_unit {
            write!(
                output,
                "work_unit,{prefix},{},{},{},{},{},{},{}\r\n",
                csv_field(row.project_id),
                csv_field(row.work_unit_id),
                row.totals.elapsed_seconds,
                csv_field(row.estimated_seconds),
                csv_field(row.variance_seconds),
                row.totals.session_count,
                row.totals.needs_review_count
            )?;
        }
        Ok(())
    }
}

fn csv_field(value: Option<impl ToString>) -> String {
    value.map_or_else(String::new, |value| value.to_string())
}

#[cfg(test)]
mod tests {
    use clap::{Parser, error::ErrorKind};
    use mcbtodo_protocol::focus::WorkUnitFocus;
    use serde_json::json;

    use super::*;
    use crate::{Cli, Command, TimerCommand};

    fn parse(args: &[&str]) -> anyhow::Result<Cli, clap::Error> {
        Cli::try_parse_from(
            ["mcbtodo", "timer", "report"]
                .into_iter()
                .chain(args.iter().copied()),
        )
    }

    fn dates(args: &[&str]) -> Result<(NaiveDate, NaiveDate)> {
        let Some(Command::Timer {
            command: TimerCommand::Report(args),
            ..
        }) = parse(args)?.command
        else {
            panic!("expected a timer report")
        };
        args.dates("2026-01-03".parse()?)
    }

    #[test]
    fn report_accepts_week_with_anchor_and_explicit_range_with_csv() {
        assert!(parse(&["--week", "--date", "2026-01-01"]).is_ok());
        assert!(
            parse(&[
                "--from",
                "2025-12-29",
                "--to",
                "2026-01-04",
                "--csv",
                "--project",
                "10000000-0000-0000-0000-000000000001",
            ])
            .is_ok()
        );
    }

    #[test]
    fn report_rejects_an_alternate_vault_before_reading_it() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let alternate_vault = dir.path().join("alternate-vault");
        std::fs::create_dir(&alternate_vault)?;
        let cli = parse(&[
            "--vault",
            alternate_vault.to_str().unwrap(),
            "--timezone",
            "UTC",
        ])?;
        let error = crate::run(cli).unwrap_err();
        assert!(error.to_string().contains("default Vault"), "{error:#}");
        Ok(())
    }

    #[test]
    fn report_rejects_partial_ranges_and_incompatible_options() {
        assert_eq!(
            parse(&["--from", "2026-01-01"]).err().unwrap().kind(),
            ErrorKind::MissingRequiredArgument
        );
        for args in [
            &["--week", "--from", "2026-01-01", "--to", "2026-01-02"][..],
            &[
                "--date",
                "2026-01-01",
                "--from",
                "2026-01-01",
                "--to",
                "2026-01-02",
            ],
            &["--csv", "--json"],
        ] {
            assert_eq!(
                parse(args).err().unwrap().kind(),
                ErrorKind::ArgumentConflict
            );
        }
    }

    #[test]
    fn report_resolves_local_monday_to_sunday_and_validates_inclusive_ranges() -> Result<()> {
        assert_eq!(dates(&[])?, ("2026-01-03".parse()?, "2026-01-03".parse()?));
        assert_eq!(
            dates(&["--week", "--date", "2026-01-04"])?,
            ("2025-12-29".parse()?, "2026-01-04".parse()?),
        );
        assert_eq!(
            dates(&["--week"])?,
            dates(&["--week", "--date", "2026-01-04"])?
        );
        assert!(dates(&["--week", "--date", "9999-12-31"]).is_err());
        assert!(dates(&["--from", "2024-12-31", "--to", "2024-01-01"]).is_err());
        assert!(dates(&["--from", "2024-01-01", "--to", "2024-12-31"]).is_ok());
        assert!(dates(&["--from", "2024-01-01", "--to", "2025-01-01"]).is_err());
        Ok(())
    }

    const PROJECT: Id = Id::from_u128(1);
    const OTHER_PROJECT: Id = Id::from_u128(2);
    const UNIT: Id = Id::from_u128(3);
    const OTHER_UNIT: Id = Id::from_u128(4);
    const REMOVED_UNIT: Id = Id::from_u128(5);
    const ZERO_ESTIMATE_UNIT: Id = Id::from_u128(6);

    fn test_queries(root: &Path) -> Result<(Vec<ProjectSummary>, Vec<WorkUnitSummary>)> {
        for (project, task, units) in [
            (
                PROJECT,
                Id::from_u128(7),
                &[(UNIT, "2"), (ZERO_ESTIMATE_UNIT, "0")][..],
            ),
            (OTHER_PROJECT, Id::from_u128(8), &[(OTHER_UNIT, "null")]),
        ] {
            let dir = root.join("projects").join(project.to_string());
            std::fs::create_dir_all(dir.join("tasks"))?;
            std::fs::create_dir_all(dir.join("units"))?;
            std::fs::write(
                dir.join("project.md"),
                format!("---\nschema: mcbtodo.project/v1\nid: {project}\n---\n\n# Project\n"),
            )?;
            std::fs::write(
                dir.join("tasks/task.md"),
                format!(
                    "---\nschema: mcbtodo.task/v1\nid: {task}\nproject_id: {project}\n---\n\n# Task\n"
                ),
            )?;
            for (unit, estimate) in units {
                std::fs::write(
                    dir.join("units").join(format!("{unit}.md")),
                    format!(
                        "---\nschema: mcbtodo.work-unit/v1\nid: {unit}\nproject_id: {project}\nparent_task_id: {task}\nestimated_minutes: {estimate}\n---\n\n# Unit\n"
                    ),
                )?;
            }
        }
        let queries = VaultQueries::new(root, chrono_tz::Asia::Shanghai);
        Ok((
            queries.list_projects()?,
            queries.list_work_units(None, None)?,
        ))
    }

    fn report_two_days(
        projects: &[ProjectSummary],
        work_units: &[WorkUnitSummary],
        project: Option<Id>,
    ) -> Result<Report> {
        let first: NaiveDate = "2026-01-04".parse()?;
        let last = "2026-01-05".parse()?;
        let mut days = [first, last].into_iter();
        let unit = |work_unit_id, elapsed_seconds, needs_review_count| WorkUnitFocus {
            work_unit_id,
            elapsed_seconds,
            session_count: 1,
            needs_review_count,
        };
        let report = collect(
            projects,
            work_units,
            chrono_tz::Asia::Shanghai,
            (first, last),
            project,
            |request| {
                let date = days.next().expect("unexpected additional request");
                assert_eq!(
                    request,
                    Request::FocusReport {
                        date,
                        timezone: "Asia/Shanghai".into()
                    }
                );
                let (total, rows) = if date == first {
                    (
                        50,
                        vec![
                            unit(None, 8, 0),
                            unit(Some(UNIT), 24, 1),
                            unit(Some(OTHER_UNIT), 17, 0),
                        ],
                    )
                } else {
                    (
                        34,
                        vec![
                            unit(Some(UNIT), 25, 0),
                            unit(Some(REMOVED_UNIT), 8, 0),
                            unit(Some(ZERO_ESTIMATE_UNIT), 1, 0),
                        ],
                    )
                };
                Ok(Response::FocusReport(DailyFocusReport {
                    date,
                    timezone: "Asia/Shanghai".into(),
                    total_elapsed_seconds: total,
                    session_count: 3,
                    needs_review_count: u64::from(date == first),
                    by_work_unit: rows,
                }))
            },
        )?;
        assert!(days.next().is_none(), "missing inclusive end-date query");
        Ok(report)
    }

    #[test]
    fn unfiltered_report_preserves_daemon_totals_and_unknown_units() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let (projects, work_units) = test_queries(dir.path())?;
        let report = serde_json::to_value(report_two_days(&projects, &work_units, None)?)?;
        assert_eq!(report["date"], "2026-01-04");
        assert_eq!(report["end_date"], "2026-01-05");
        assert_eq!(report["timezone"], "Asia/Shanghai");
        assert_eq!(report["total_elapsed_seconds"], 84);
        assert_eq!(report["session_count"], 6);
        assert_eq!(report["needs_review_count"], 1);
        let rows = report["by_work_unit"].as_array().unwrap();
        assert_eq!(rows.len(), 5);
        assert!(rows[0]["work_unit_id"].is_null());
        assert_eq!(rows[2]["project_id"], json!(OTHER_PROJECT));
        assert!(rows[2]["variance_seconds"].is_null());
        assert_eq!(rows[3]["work_unit_id"], json!(REMOVED_UNIT));
        assert!(rows[3]["project_id"].is_null());
        Ok(())
    }

    #[test]
    fn project_report_filters_by_membership_and_counts_each_estimate_once() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let (projects, work_units) = test_queries(dir.path())?;
        let report = report_two_days(&projects, &work_units, Some(PROJECT))?;
        assert_eq!(report.total_elapsed_seconds, 50);
        assert_eq!(report.session_count, 3);
        assert_eq!(report.needs_review_count, 1);
        assert_eq!(report.by_work_unit.len(), 2);
        let row = &report.by_work_unit[0];
        assert_eq!(row.work_unit_id, Some(UNIT));
        assert_eq!(row.totals.elapsed_seconds, 49);
        assert_eq!(row.totals.session_count, 2);
        assert_eq!(row.totals.needs_review_count, 1);
        assert_eq!(row.estimated_seconds, Some(120));
        assert_eq!(row.variance_seconds, Some(-71));
        let row = &report.by_work_unit[1];
        assert_eq!(row.work_unit_id, Some(ZERO_ESTIMATE_UNIT));
        assert_eq!(row.estimated_seconds, Some(0));
        assert_eq!(row.variance_seconds, Some(1));
        assert!(
            collect(
                &projects,
                &work_units,
                chrono_tz::Asia::Shanghai,
                (report.date, report.end_date),
                Some(UNIT),
                |_| panic!("requested daemon report for invalid project"),
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn csv_retains_summary_counts_and_optional_fields() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let (projects, work_units) = test_queries(dir.path())?;
        let report = report_two_days(&projects, &work_units, None)?;
        let mut output = Vec::new();
        report.write_csv(&mut output)?;
        let output = String::from_utf8(output)?;
        assert!(output.ends_with("\r\n"));
        let rows: Vec<_> = output.split_terminator("\r\n").collect();
        assert!(rows.iter().all(|row| row.split(',').count() == 11));
        assert_eq!(rows.len(), 7);
        assert_eq!(
            rows[1],
            "total,2026-01-04,2026-01-05,Asia/Shanghai,,,84,,,6,1"
        );
        assert_eq!(
            rows[2],
            "work_unit,2026-01-04,2026-01-05,Asia/Shanghai,,,8,,,1,0"
        );
        assert_eq!(
            rows[3],
            format!(
                "work_unit,2026-01-04,2026-01-05,Asia/Shanghai,{PROJECT},{UNIT},49,120,-71,2,1"
            )
        );
        Ok(())
    }

    #[test]
    fn mismatched_report_date_fails_the_report() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let (projects, work_units) = test_queries(dir.path())?;
        let first = "2026-01-04".parse()?;
        let result = collect(
            &projects,
            &work_units,
            chrono_tz::Asia::Shanghai,
            (first, first),
            None,
            |_| {
                Ok(Response::FocusReport(DailyFocusReport {
                    date: "2026-01-05".parse()?,
                    timezone: "Asia/Shanghai".into(),
                    total_elapsed_seconds: 0,
                    session_count: 0,
                    needs_review_count: 0,
                    by_work_unit: Vec::new(),
                }))
            },
        );
        assert!(result.is_err());
        Ok(())
    }
}
