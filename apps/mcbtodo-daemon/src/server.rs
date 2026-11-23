use std::io;
use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result};
use chrono::Datelike;
use chrono_tz::Tz;
use mcbtodo_application::VaultCommands;
use mcbtodo_execution::notification::NotifySend;
use mcbtodo_platform::WorkspacePaths;
use mcbtodo_protocol::{ErrorCode, Request, Response};
use mcbtodo_storage_sqlite::focus::FocusStore;
use mcbtodo_storage_sqlite::run_store::RunStore;
use mcbtodo_vault::WriteError;
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::io::Errno;
use uuid::Uuid;

use crate::endpoint::Endpoint;
use crate::notifications::{DesktopNotifications, reminders::ReminderPoller};
use crate::runtime::Runtime;
use crate::timer::Timer;
use crate::worker_process::LinuxWorkerLauncher;

mod connection;

use connection::{Connection, ConnectionEvent};

pub struct Server {
    connection: Option<Connection>,
    endpoint: Endpoint,
    timer: Timer,
    runtime: Runtime,
    desktop: Option<DesktopNotifications>,
    reminders: ReminderPoller,
}

impl Server {
    pub fn open(paths: WorkspacePaths, worker_slots: u8) -> Result<Self> {
        let state_dir = paths.state_dir();
        let socket_path = paths.socket_path();
        let lock_path = paths.daemon_lock_path();
        Self::open_components(&state_dir, &socket_path, &lock_path, paths, worker_slots)
    }

    #[cfg(test)]
    fn open_with_paths(
        state_dir: &Path,
        socket_path: &Path,
        paths: WorkspacePaths,
    ) -> Result<Self> {
        let lock_path = state_dir.join("mcbtodo-daemon.lock");
        Self::open_components(
            state_dir,
            socket_path,
            &lock_path,
            paths,
            crate::DEFAULT_WORKER_SLOTS,
        )
    }

    fn open_components(
        state_dir: &Path,
        socket_path: &Path,
        lock_path: &Path,
        paths: WorkspacePaths,
        worker_slots: u8,
    ) -> Result<Self> {
        let endpoint = Endpoint::bind(state_dir, socket_path, lock_path)?;
        let path = if state_dir == paths.state_dir() {
            paths.focus_database_path()
        } else {
            state_dir.join("focus.sqlite3")
        };
        let (focus, recovered) =
            FocusStore::open(&path).with_context(|| format!("opening {}", path.display()))?;
        let timer = Timer::new(focus, recovered);
        let workflow_path = if state_dir == paths.state_dir() {
            paths.workflow_database_path()
        } else {
            state_dir.join("workflow.sqlite3")
        };
        let workflow = RunStore::open(&workflow_path)
            .with_context(|| format!("opening {}", workflow_path.display()))?;
        let runtime = Runtime::new(
            state_dir,
            paths,
            workflow,
            LinuxWorkerLauncher,
            worker_slots,
        )?;
        let reminders = ReminderPoller::start()?;
        let desktop = if let Some(sink) = NotifySend::for_session() {
            match DesktopNotifications::start(runtime.notifications(), sink) {
                Ok(desktop) => {
                    runtime.notifications().recover_reminder_deliveries()?;
                    Some(desktop)
                }
                Err(error) => {
                    eprintln!("mcbtodo-daemon: starting desktop notifications: {error:#}");
                    None
                }
            }
        } else {
            None
        };
        Ok(Self {
            connection: None,
            endpoint,
            timer,
            runtime,
            desktop,
            reminders,
        })
    }

    pub fn run(mut self) -> Result<()> {
        loop {
            self.step()?;
        }
    }

    fn step(&mut self) -> Result<()> {
        let focus_wait = self
            .timer
            .checkpoint_due()
            .map(|due| due.saturating_duration_since(Instant::now()));
        let runtime_wait = self.runtime.wait_timeout()?;
        let timeout = focus_wait
            .into_iter()
            .chain(runtime_wait)
            .chain(self.reminders.wait_timeout())
            .min()
            .map(|left| Timespec {
                tv_sec: left.as_secs() as i64,
                tv_nsec: left.subsec_nanos().into(),
            });
        let mut fds = [match &self.connection {
            Some(connection) => PollFd::new(connection.wake(), PollFlags::IN),
            None => PollFd::new(&self.endpoint.listener, PollFlags::IN),
        }];
        let ready = match poll(&mut fds, timeout.as_ref()) {
            Err(Errno::INTR) => return Ok(()),
            Err(error) => return Err(error).context("waiting for a daemon connection"),
            Ok(ready) => ready,
        };
        self.timer
            .checkpoint(Instant::now())
            .context("checkpointing focus timer")?;
        self.runtime.tick().context("advancing workflow queue")?;
        let (generation, reminders) = self.runtime.reminder_snapshot();
        match self
            .reminders
            .tick(self.runtime.notifications(), generation, reminders)
        {
            Ok(_published) => {
                if let Some(desktop) = &mut self.desktop
                    && let Err(error) = desktop.dispatch_reminders(self.runtime.notifications())
                {
                    eprintln!("mcbtodo-daemon: dispatching reminder deliveries: {error:#}");
                }
            }
            Err(error) => eprintln!("mcbtodo-daemon: publishing reminders: {error:#}"),
        }
        if ready != 0 {
            self.handle_ipc_event()?;
        }
        if let Some(desktop) = &mut self.desktop
            && let Err(error) = desktop.dispatch(self.runtime.notifications())
        {
            eprintln!("mcbtodo-daemon: dispatching desktop notifications: {error:#}");
        }
        Ok(())
    }

    fn handle_ipc_event(&mut self) -> Result<()> {
        if let Some(mut connection) = self.connection.take() {
            if let ConnectionEvent::Request(request) = connection.event()? {
                connection.reply(self.reply(request));
                self.connection = Some(connection);
            }
        } else {
            match self.endpoint.listener.accept() {
                Ok((stream, _)) => match Connection::start(stream) {
                    Ok(connection) => self.connection = Some(connection),
                    Err(error) => eprintln!("mcbtodo-daemon: starting local connection: {error}"),
                },
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                    ) => {}
                Err(error) => return Err(error).context("accepting daemon connection"),
            }
        }
        Ok(())
    }

    fn reply(&mut self, request: Request) -> Response {
        match request {
            Request::Workflow(command) => self.runtime.handle(command),
            Request::CaptureTask {
                task_id,
                project_id,
                project_revision,
                title,
            } => capture_task(
                self.runtime.paths(),
                task_id,
                project_id,
                &project_revision,
                &title,
            ),
            Request::ListNotifications { before } => {
                match self.runtime.notifications().workflow_page(before) {
                    Ok(page) => Response::Notifications(page),
                    Err(error) => {
                        eprintln!("mcbtodo-daemon: loading notifications: {error:#}");
                        Response::Error {
                            code: ErrorCode::Internal,
                            message: "could not load notifications".into(),
                        }
                    }
                }
            }
            Request::ListReminderNotifications { before } => {
                match self.runtime.notifications().reminder_page(before) {
                    Ok(page) => Response::ReminderNotifications(page),
                    Err(error) => {
                        eprintln!("mcbtodo-daemon: loading reminder notifications: {error:#}");
                        Response::Error {
                            code: ErrorCode::Internal,
                            message: "could not load reminder notifications".into(),
                        }
                    }
                }
            }
            Request::FocusReport { date, timezone } => match timezone.parse::<Tz>() {
                Ok(zone) if (1..=9999).contains(&date.year()) => {
                    match self.timer.daily_report(date, zone, Instant::now()) {
                        Ok(report) => Response::FocusReport(report),
                        Err(error) => {
                            eprintln!("mcbtodo-daemon: focus report: {error:#}");
                            Response::Error {
                                code: ErrorCode::Internal,
                                message: "could not load focus history".into(),
                            }
                        }
                    }
                }
                _ => Response::Error {
                    code: ErrorCode::InvalidRequest,
                    message: "expected an IANA timezone and a date with a year from 0001 to 9999"
                        .into(),
                },
            },
            command => match self.timer.apply(command, Instant::now()) {
                Ok(Ok(status)) => Response::Status(status),
                Ok(Err(message)) => Response::Error {
                    code: ErrorCode::Conflict,
                    message: message.into(),
                },
                Err(error) => {
                    eprintln!("mcbtodo-daemon: focus timer: {error:#}");
                    Response::Error {
                        code: ErrorCode::Internal,
                        message: "could not persist focus timer".into(),
                    }
                }
            },
        }
    }
}

fn capture_task(
    paths: &WorkspacePaths,
    task_id: Uuid,
    project_id: Uuid,
    project_revision: &str,
    title: &str,
) -> Response {
    let timezone = match paths.load_config() {
        Ok(config) => config.timezone,
        Err(error) => {
            eprintln!(
                "mcbtodo-daemon: loading workspace configuration for Task capture: {error:#}"
            );
            return Response::Error {
                code: ErrorCode::Internal,
                message: "could not load workspace configuration".into(),
            };
        }
    };
    let result = VaultCommands::new(paths.vault_root(), timezone).try_capture_task(
        task_id,
        project_id,
        title,
        project_revision,
    );
    let error = match result {
        Ok(_) => {
            return Response::TaskCaptured {
                task_id,
                synced: true,
            };
        }
        Err(WriteError::PublishedButNotSynced { source, .. }) => {
            eprintln!("mcbtodo-daemon: syncing published Task {task_id}: {source}");
            return Response::TaskCaptured {
                task_id,
                synced: false,
            };
        }
        Err(error) => error,
    };
    let (code, message) = match &error {
        WriteError::StaleRevision { .. } | WriteError::ProjectNotFound { .. } => {
            (ErrorCode::StaleRevision, "Project changed".into())
        }
        WriteError::DuplicateId { .. } | WriteError::DestinationExists { .. } => (
            ErrorCode::Conflict,
            format!("Task ID {task_id} is already in use"),
        ),
        WriteError::Storage(cause)
            if cause
                .downcast_ref::<io::Error>()
                .is_some_and(|error| error.kind() == io::ErrorKind::WouldBlock) =>
        {
            (ErrorCode::Conflict, "Vault is busy".into())
        }
        WriteError::InvalidDocument(_) => (ErrorCode::InvalidRequest, error.to_string()),
        _ => {
            eprintln!("mcbtodo-daemon: capturing Task {task_id}: {error:#}");
            (
                ErrorCode::Internal,
                "could not save Task to the Vault".into(),
            )
        }
    };
    Response::Error { code, message }
}

#[cfg(test)]
#[path = "server_tests.rs"]
mod tests;
