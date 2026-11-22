use std::{
    env,
    ffi::OsString,
    path::PathBuf,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use mcbtodo_ports::{NotificationError, NotificationSink};
use rustix::fs::{Access, access};

const SYSTEM_HELPER: &str = "notify-send";

/// Optional Linux desktop notification adapter.
pub struct NotifySend {
    helper: OsString,
}

impl NotifySend {
    pub fn new(helper: OsString) -> Self {
        Self { helper }
    }

    /// Enables the system adapter when the process has a configured session bus.
    pub fn for_session() -> Option<Self> {
        env::var_os("DBUS_SESSION_BUS_ADDRESS")
            .filter(|address| !address.is_empty())
            .map(|_| Self::new(SYSTEM_HELPER.into()))
    }

    /// Passively checks for an executable helper in the current process environment.
    /// This does not establish desktop service availability or delivery.
    pub fn check_installation(&self) -> Result<PathBuf, NotificationError> {
        let search_path = env::var_os("PATH").unwrap_or_else(|| "/bin:/usr/bin".into());
        env::split_paths(&search_path)
            .map(|directory| directory.join(&self.helper))
            .find(|path| path.is_file() && access(path, Access::EXEC_OK).is_ok())
            .ok_or_else(|| {
                NotificationError::Unavailable("No executable notify-send found on PATH".into())
            })
    }
}

impl NotificationSink for NotifySend {
    fn deliver(&self, title: &str, body: &str, timeout: Duration) -> Result<(), NotificationError> {
        // The desktop service may interpret body text as markup.
        let body = body
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;");
        let started = Instant::now();
        let mut child = Command::new(&self.helper)
            .args(["--app-name=mcbtodo", "--", title, &body])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| {
                NotificationError::Unavailable(format!(
                    "could not start notification helper: {error}"
                ))
            })?;
        loop {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => return Ok(()),
                Ok(Some(status)) => {
                    return Err(NotificationError::DeliveryFailed(format!(
                        "notification helper exited with {status}"
                    )));
                }
                Ok(None) => {
                    let remaining = timeout.saturating_sub(started.elapsed());
                    if remaining.is_zero() {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(NotificationError::TimedOut);
                    }
                    thread::sleep(remaining.min(Duration::from_millis(25)));
                }
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(NotificationError::DeliveryFailed(format!(
                        "could not wait for notification helper: {error}"
                    )));
                }
            }
        }
    }
}
