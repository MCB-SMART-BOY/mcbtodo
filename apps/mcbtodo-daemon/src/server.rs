use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::Datelike;
use chrono_tz::Tz;
use mcbtodo_platform::WorkspacePaths;
use mcbtodo_protocol::{ErrorCode, ProtocolError, Request, Response, wire};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::io::Errno;
use rustix::net::sockopt::socket_peercred;
use rustix::process::geteuid;

use crate::endpoint::Endpoint;
use crate::notifications::{DesktopNotifications, reminders::ReminderPoller};
use crate::runtime::Runtime;
use crate::timer::Timer;

const TRANSFER_TIMEOUT: Duration = Duration::from_secs(1);

pub struct Server {
    endpoint: Endpoint,
    timer: Timer,
    runtime: Runtime,
    desktop: Option<DesktopNotifications>,
    reminders: ReminderPoller,
}

impl Server {
    pub fn open(paths: WorkspacePaths) -> Result<Self> {
        let state_dir = paths.state_dir();
        let socket_path = paths.socket_path();
        let lock_path = paths.daemon_lock_path();
        Self::open_components(&state_dir, &socket_path, &lock_path, paths)
    }

    #[cfg(test)]
    fn open_with_paths(
        state_dir: &Path,
        socket_path: &Path,
        paths: WorkspacePaths,
    ) -> Result<Self> {
        let lock_path = state_dir.join("mcbtodo-daemon.lock");
        Self::open_components(state_dir, socket_path, &lock_path, paths)
    }

    fn open_components(
        state_dir: &Path,
        socket_path: &Path,
        lock_path: &Path,
        paths: WorkspacePaths,
    ) -> Result<Self> {
        let endpoint = Endpoint::bind(state_dir, socket_path, lock_path)?;
        let path = if state_dir == paths.state_dir() {
            paths.focus_database_path()
        } else {
            state_dir.join("focus.sqlite3")
        };
        let timer = Timer::open(&path).with_context(|| format!("opening {}", path.display()))?;
        #[cfg(test)]
        let runtime = Runtime::open_with_paths(state_dir, paths.clone())?;
        #[cfg(not(test))]
        let runtime = Runtime::open(paths.clone())?;
        let reminders = ReminderPoller::start()?;
        let desktop = if std::env::var_os("DBUS_SESSION_BUS_ADDRESS")
            .is_some_and(|address| !address.is_empty())
        {
            match DesktopNotifications::start(runtime.notifications(), "notify-send".into()) {
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
        let mut fds = [PollFd::new(&self.endpoint.listener, PollFlags::IN)];
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
            match self.endpoint.listener.accept() {
                Ok((stream, _)) => self.handle(stream),
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                    ) => {}
                Err(error) => return Err(error).context("accepting daemon connection"),
            }
        }
        if let Some(desktop) = &mut self.desktop
            && let Err(error) = desktop.dispatch(self.runtime.notifications())
        {
            eprintln!("mcbtodo-daemon: dispatching desktop notifications: {error:#}");
        }
        Ok(())
    }

    fn handle(&mut self, stream: UnixStream) {
        match socket_peercred(&stream) {
            Ok(credential) if credential.uid == geteuid() => (),
            Ok(_) => return,
            Err(error) => {
                eprintln!("mcbtodo-daemon: checking peer credentials: {error}");
                return;
            }
        }
        let mut stream = TimedStream {
            stream,
            deadline: Instant::now() + TRANSFER_TIMEOUT,
        };
        let reply = match wire::read::<Request>(&mut stream) {
            Ok(Request::Workflow(command)) => self.runtime.handle(command),
            Ok(Request::ListNotifications { before }) => {
                match self.runtime.notifications().page(before) {
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
            Ok(Request::ListReminderNotifications { before }) => {
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
            Ok(Request::FocusReport { date, timezone }) => match timezone.parse::<Tz>() {
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
            Ok(command) => match self.timer.apply(command, Instant::now()) {
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
            Err(ProtocolError::Io(_)) => return,
            Err(error) => Response::Error {
                code: if matches!(error, ProtocolError::UnsupportedVersion(_)) {
                    ErrorCode::UnsupportedVersion
                } else {
                    ErrorCode::InvalidRequest
                },
                message: error.to_string(),
            },
        };
        stream.deadline = Instant::now() + TRANSFER_TIMEOUT;
        if let Err(error) = wire::write(&mut stream, &reply) {
            eprintln!("mcbtodo-daemon: sending response: {error}");
        }
    }
}

// Bound an entire frame transfer even if a client sends a few bytes at a time.
struct TimedStream {
    stream: UnixStream,
    deadline: Instant,
}

impl TimedStream {
    fn remaining(&self) -> io::Result<Duration> {
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "IPC transfer timed out"))
    }
}

impl Read for TimedStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.stream.set_read_timeout(Some(self.remaining()?))?;
        self.stream.read(buf)
    }
}

impl Write for TimedStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.stream.set_write_timeout(Some(self.remaining()?))?;
        self.stream.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

#[cfg(test)]
#[path = "server_tests.rs"]
mod tests;
