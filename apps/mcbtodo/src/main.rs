mod agent_discovery;
#[cfg_attr(not(unix), path = "backup_unsupported.rs")]
mod backup;
mod calendar_format;
mod daemon_client;
mod doctor;
mod focus_report;
mod ical_import;
mod mcp;
mod proposal;
mod tui;
mod vault_cli;
mod workflow;
mod workflow_runtime_cli;

use std::{
    io,
    path::{Path, PathBuf},
    process::ExitCode,
};

use anyhow::{Context, Result, bail, ensure};
use chrono::{NaiveDate, Utc};
use chrono_tz::Tz;
use clap::{Parser, Subcommand};
use icalendar::{Calendar as IcalCalendar, Component, Event as IcalEvent, EventLike, Todo};
use mcbtodo_application::{CalendarItem, VaultCommands, VaultQueries};
use mcbtodo_domain::{
    Id, Occurrence,
    time::{DateParseError, Deadline, parse_date},
};
use mcbtodo_platform::{AppConfig, WorkspacePaths};
use mcbtodo_protocol::{Request, Response};
use mcbtodo_vault::Vault;

#[derive(Parser)]
#[command(version, about = "Markdown-first calendar and task workspace")]
struct Cli {
    /// Use a Markdown vault other than ~/.mcbtodo/vault.
    #[arg(long, global = true, value_name = "PATH")]
    vault: Option<PathBuf>,
    /// Override the configured IANA display timezone for this invocation.
    #[arg(long, global = true, value_name = "ZONE")]
    timezone: Option<Tz>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Initialize the application directories and config file without overwriting them.
    Init,
    /// Verify front matter, references, time ranges and the work-unit DAG.
    Validate,
    /// Diagnose workspace paths, the Vault, and the daemon socket without changing state.
    #[command(
        after_help = "The Linux daemon check looks for a listening socket without connecting. \
        A stopped daemon and missing optional prerequisites do not affect the exit status."
    )]
    Doctor {
        /// Print a JSON report. Workspace errors still cause a nonzero exit.
        #[arg(long)]
        json: bool,
    },
    /// Make atomic changes to the Markdown vault.
    Vault {
        #[command(subcommand)]
        command: vault_cli::Command,
    },
    /// List projects, tasks and work units.
    Outline,
    /// Show the selected day's calendar projection.
    Calendar {
        /// Query the Linux daemon's default Vault for this day.
        #[arg(long)]
        daemon: bool,
        #[arg(long, value_parser = parse_day)]
        date: Option<NaiveDate>,
        #[arg(long)]
        json: bool,
        #[command(subcommand)]
        export: Option<CalendarExport>,
    },
    /// Export the work-unit DAG as Mermaid.
    Graph,
    /// Open the keyboard-driven workspace.
    Tui {
        #[arg(long, value_parser = parse_day)]
        date: Option<NaiveDate>,
    },
    /// Control the persistent focus timer through the local daemon.
    Timer {
        #[command(subcommand)]
        command: TimerCommand,
        #[arg(long, global = true)]
        json: bool,
    },
    /// Validate, publish, schedule and inspect workflows.
    Workflow {
        #[command(subcommand)]
        command: workflow::Command,
    },
    /// List persisted notifications as JSON, newest first.
    Notifications {
        /// Show reminder deliveries instead of workflow outcomes.
        #[arg(long)]
        reminders: bool,
        /// Continue from the next_before cursor returned by the previous page.
        #[arg(long, value_name = "UUID")]
        before: Option<uuid::Uuid>,
    },
    /// Expose the configured Vault over Model Context Protocol.
    Mcp {
        #[command(subcommand)]
        command: McpCommand,
    },
    /// Review proposed Vault changes.
    Proposal {
        #[command(subcommand)]
        command: proposal::Command,
    },
    /// Back up the configured workspace while mcbtodo-daemon is stopped.
    Backup { destination: PathBuf },
    /// Restore a backup into a new home directory.
    Restore {
        source: PathBuf,
        destination_home: PathBuf,
    },
}

#[derive(Subcommand)]
enum CalendarExport {
    /// Export calendar projections in an inclusive date window as RFC 5545 iCalendar.
    ExportIcal {
        #[arg(long, value_parser = parse_day)]
        from: NaiveDate,
        #[arg(long, value_parser = parse_day)]
        through: NaiveDate,
        #[arg(long, value_name = "PATH")]
        output: Option<PathBuf>,
    },
    /// Import supported VEVENT components as independent Markdown calendar events.
    ImportIcal {
        /// Input iCalendar file.
        input: PathBuf,
    },
}

#[derive(Subcommand)]
enum McpCommand {
    /// Serve Vault queries and proposal tools over stdio.
    Serve,
}

#[derive(Subcommand)]
enum TimerCommand {
    Status,
    Start {
        /// Associate the session with a WorkUnit in the selected vault.
        #[arg(long)]
        unit: Option<Id>,
    },
    Pause,
    Stop,
    /// Show focus time and estimates by day, week or date range.
    Report(focus_report::ReportArgs),
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("mcbtodo: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    let command = cli.command.unwrap_or(Command::Tui { date: None });
    if let Command::Restore {
        source,
        destination_home,
    } = command
    {
        backup::restore(&source, &destination_home)?;
        println!("Restored workspace to {}", destination_home.display());
        return Ok(());
    }
    let location = VaultLocation {
        root_override: cli.vault,
        timezone_override: cli.timezone,
    };
    if let Command::Doctor { json } = command {
        return doctor::run(WorkspacePaths::discover(), location, json);
    }
    let paths = WorkspacePaths::discover()?;
    match command {
        Command::Tui { date } => {
            let has_timezone_override = location.timezone_override.is_some();
            let Some((root, timezone, config)) = location.resolve_tui(&paths)? else {
                return Ok(());
            };
            let vault = Vault::load(&root, timezone)?;
            let day = date.unwrap_or_else(|| today(&vault));
            tui::run(&root, vault, day, &paths, config, has_timezone_override)?;
        }
        Command::Workflow { command } => workflow::run(command, &paths)?,
        Command::Init => {
            paths.initialize()?;
            println!(
                "Config: {}\nVault: {}",
                paths.config_path().display(),
                paths.vault_root().display()
            );
        }
        Command::Timer { command, json } => {
            if let TimerCommand::Report(args) = command {
                let (root, timezone) = location.resolve(&paths)?;
                require_default_focus_vault(&paths, &root)?;
                return focus_report::run(&paths, &root, timezone, args, json);
            }
            let command = match command {
                TimerCommand::Status => Request::Status,
                TimerCommand::Start { unit } => {
                    if let Some(id) = unit {
                        let (root, zone) = location.resolve(&paths)?;
                        require_default_focus_vault(&paths, &root)?;
                        let queries = VaultQueries::new(&root, zone);
                        ensure!(
                            queries.contains_work_unit(id)?,
                            "no work unit with ID {id} in the vault"
                        );
                    }
                    Request::Start { work_unit_id: unit }
                }
                TimerCommand::Pause => Request::Pause,
                TimerCommand::Stop => Request::Stop,
                TimerCommand::Report(_) => unreachable!("handled above"),
            };
            let status = daemon_client::timer(&paths, command)?;
            if json {
                serde_json::to_writer_pretty(io::stdout().lock(), &status)?;
                println!();
            } else {
                println!("{}", focus_report::timer_label(&status));
            }
        }
        Command::Mcp {
            command: McpCommand::Serve,
        } => {
            let (root, timezone) = location.resolve(&paths)?;
            mcp::serve(&root, timezone, &paths)?;
        }
        Command::Notifications { before, reminders } => {
            let request = if reminders {
                Request::ListReminderNotifications { before }
            } else {
                Request::ListNotifications { before }
            };
            match daemon_client::exchange(&paths, request)? {
                Response::Notifications(page) if !reminders => {
                    serde_json::to_writer_pretty(io::stdout().lock(), &page)?;
                }
                Response::ReminderNotifications(page) if reminders => {
                    serde_json::to_writer_pretty(io::stdout().lock(), &page)?;
                }
                _ => bail!("unexpected response to notifications request"),
            }
            println!();
        }
        Command::Proposal { command } => {
            let (root, timezone) = location.resolve(&paths)?;
            proposal::run(command, &root, timezone, &paths)?;
        }
        Command::Backup { destination } => {
            ensure!(
                location.root_override.is_none(),
                "backup operates on the default Vault"
            );
            backup::backup(&paths, &destination)?;
            println!("Backed up workspace to {}", destination.display());
        }
        Command::Vault { command } => {
            let (root, timezone) = location.resolve(&paths)?;
            if vault_cli::requires_snapshot(&command) {
                let vault = Vault::load(&root, timezone)?;
                vault_cli::run(command, &root, &vault, &paths)?;
            } else {
                vault_cli::run_without_snapshot(command, &root, timezone)?;
            }
        }
        Command::Outline => {
            let (root, timezone) = location.resolve(&paths)?;
            let rows = VaultQueries::new(root, timezone).outline()?;
            for row in rows {
                let blocked = if row.blocked { " [blocked]" } else { "" };
                println!(
                    "{}{title}{blocked}",
                    "  ".repeat(row.depth),
                    title = row.title
                );
            }
        }
        Command::Graph => {
            let (root, timezone) = location.resolve(&paths)?;
            print!(
                "{}",
                VaultQueries::new(root, timezone).work_unit_graph_mermaid()?
            );
        }
        Command::Validate => {
            let (root, timezone) = location.resolve(&paths)?;
            VaultQueries::new(root, timezone).validate()?;
            println!("Vault is valid ({timezone})");
        }
        Command::Calendar {
            daemon,
            export:
                Some(CalendarExport::ExportIcal {
                    from,
                    through,
                    output,
                }),
            ..
        } => {
            ensure!(
                !daemon,
                "--daemon is only supported for calendar day queries"
            );
            let (root, timezone) = location.resolve(&paths)?;
            let window = mcbtodo_domain::time::DateSpan::new(from, through)?;
            let entries = VaultQueries::new(root, timezone).calendar(window)?;
            export_ical(entries, timezone, output.as_deref())?;
        }
        Command::Calendar {
            daemon,
            date,
            json,
            export: None,
        } => {
            ensure!(
                !daemon || location.root_override.is_none(),
                "--daemon queries the default Vault and cannot be combined with --vault"
            );
            let (root, timezone) = location.resolve(&paths)?;
            let date = date.unwrap_or_else(|| Utc::now().with_timezone(&timezone).date_naive());
            let entries = if daemon {
                daemon_client::calendar(
                    &paths,
                    mcbtodo_domain::time::DateSpan::new(date, date)?,
                    timezone,
                )?
            } else {
                VaultQueries::new(root, timezone).calendar_day(date)?
            };
            if json {
                serde_json::to_writer_pretty(io::stdout().lock(), &entries)?;
                println!();
            } else {
                println!("{date} · {timezone}");
                for entry in &entries {
                    println!("{}", calendar_format::item_label(entry, timezone));
                }
            }
        }
        Command::Calendar {
            daemon,
            export: Some(CalendarExport::ImportIcal { input }),
            ..
        } => {
            ensure!(
                !daemon,
                "--daemon is only supported for calendar day queries"
            );
            let (root, timezone) = location.resolve(&paths)?;
            Vault::load(&root, timezone)?;
            let imported = ical_import::parse_file(&input)?;
            let count = imported.len();
            VaultCommands::new(&root, timezone)
                .create_events_atomic(imported)
                .map_err(anyhow::Error::new)?;
            println!("Imported {count} calendar events");
        }
        Command::Doctor { .. } | Command::Restore { .. } => unreachable!("handled above"),
    }
    Ok(())
}

struct VaultLocation {
    root_override: Option<PathBuf>,
    timezone_override: Option<Tz>,
}

impl VaultLocation {
    fn resolve_tui(self, paths: &WorkspacePaths) -> Result<Option<(PathBuf, Tz, AppConfig)>> {
        let snapshot = if self.root_override.is_some() {
            paths.read_app_config_snapshot()?
        } else {
            paths.initialize_config_snapshot()?
        };
        let config = match snapshot.preferences() {
            Some(config) => config,
            None => match tui::repair_config(paths, snapshot)? {
                Some(config) => config,
                None => return Ok(None),
            },
        };
        let timezone = self.timezone_override.unwrap_or(config.timezone);
        Ok(Some((self.root(paths), timezone, config)))
    }

    fn resolve(self, paths: &WorkspacePaths) -> Result<(PathBuf, Tz)> {
        let timezone = match self.timezone_override {
            Some(timezone) => timezone,
            None if self.root_override.is_some() => {
                paths.load_optional_config()?.unwrap_or_default().timezone
            }
            None => paths.load_config()?.timezone,
        };
        Ok((self.root(paths), timezone))
    }

    fn root(self, paths: &WorkspacePaths) -> PathBuf {
        self.root_override.unwrap_or_else(|| paths.vault_root())
    }
}

pub(crate) fn require_default_focus_vault(paths: &WorkspacePaths, root: &Path) -> Result<()> {
    let default = paths.vault_root();
    if root != default {
        ensure!(
            default.try_exists()? && root.canonicalize()? == default.canonicalize()?,
            "focus sessions can only reference WorkUnits in the default Vault ({})",
            default.display(),
        );
    }
    Ok(())
}

pub(crate) fn today(vault: &Vault) -> NaiveDate {
    Utc::now().with_timezone(&vault.timezone()).date_naive()
}

fn export_ical(entries: Vec<CalendarItem>, timezone: Tz, output: Option<&Path>) -> Result<()> {
    let mut calendar = IcalCalendar::new().name("mcbtodo").done();
    for entry in entries {
        let kind = entry.kind.label();
        match entry.occurrence {
            Occurrence::Span(mcbtodo_domain::time::Span::DateSpan(span)) => {
                let end = span
                    .through()
                    .succ_opt()
                    .context("date span cannot be represented as an exclusive iCalendar end")?;
                let uid = format!("urn:uuid:{}:{kind}:{}", entry.id, span.start());
                let mut event = IcalEvent::with_uid(&uid);
                event
                    .summary(&entry.title)
                    .starts(span.start())
                    .ends(end)
                    .add_property("X-MCBTODO-KIND", kind)
                    .add_property("X-MCBTODO-ID", entry.id.to_string());
                calendar.push(event.done());
            }
            Occurrence::Span(mcbtodo_domain::time::Span::DateTimeSpan(span)) => {
                let uid = format!("urn:uuid:{}:{kind}:{}", entry.id, span.start().to_utc());
                let mut event = IcalEvent::with_uid(&uid);
                event
                    .summary(&entry.title)
                    .starts(span.start().to_utc())
                    .ends(span.end().to_utc())
                    .add_property("X-MCBTODO-KIND", kind)
                    .add_property("X-MCBTODO-ID", entry.id.to_string());
                calendar.push(event.done());
            }
            Occurrence::Deadline(Deadline::Date { value }) => {
                let mut todo = Todo::with_uid(&format!("urn:uuid:{}:due:{value}", entry.id));
                todo.summary(&entry.title)
                    .due(value)
                    .add_property("X-MCBTODO-KIND", kind)
                    .add_property("X-MCBTODO-ID", entry.id.to_string());
                calendar.push(todo.done());
            }
            Occurrence::Deadline(deadline @ Deadline::DateTime(_)) => {
                let instant = deadline
                    .datetime(timezone)
                    .context("date-time deadline has no instant")?
                    .to_utc();
                let mut todo = Todo::with_uid(&format!("urn:uuid:{}:due:{instant}", entry.id));
                todo.summary(&entry.title)
                    .due(instant)
                    .add_property("X-MCBTODO-KIND", kind)
                    .add_property("X-MCBTODO-ID", entry.id.to_string());
                calendar.push(todo.done());
            }
        }
    }
    let serialized = calendar.to_string();
    if let Some(path) = output {
        std::fs::write(path, serialized)
            .with_context(|| format!("writing iCalendar export to {}", path.display()))?;
    } else {
        print!("{serialized}");
    }
    Ok(())
}

pub(crate) fn parse_day(value: &str) -> Result<NaiveDate, String> {
    parse_date(value).map_err(|error| match error {
        DateParseError::Format => "expected a date in YYYY-MM-DD format".into(),
        DateParseError::YearRange => "year must be between 0001 and 9999".into(),
    })
}
