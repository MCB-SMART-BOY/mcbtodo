use std::{
    io::{self, Write},
    os::unix::process::CommandExt,
    path::Path,
    process::{Child, ChildStdin, Command, Stdio},
    time::Instant,
};

use mcbtodo_ports::process::{
    LaunchedWorker, ProcessController, ProcessLauncher, WorkerExit, WorkerInput,
};
use rustix::{
    event::{PollFd, PollFlags, Timespec, poll},
    fs::{OFlags, fcntl_getfl, fcntl_setfl},
    io::Errno,
    process::{Pid, Signal, WaitId, WaitIdOptions, getpid, kill_process_group, waitid},
};

pub(crate) struct LinuxWorkerLauncher;

impl ProcessLauncher for LinuxWorkerLauncher {
    fn launch_worker(&self, executable: &Path, home: &Path) -> io::Result<LaunchedWorker> {
        let child = Command::new(executable)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .env_clear()
            .env("HOME", home)
            .env("MCBTODO_PARENT_PID", getpid().as_raw_pid().to_string())
            .process_group(0)
            .spawn()?;
        let mut process = LinuxWorkerProcess::new(child);
        let stdin = process
            .child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("worker stdin was not piped"))?;
        let stdout = process
            .child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("worker stdout was not piped"))?;
        Ok(LaunchedWorker {
            process: Box::new(process),
            stdin: Box::new(LinuxWorkerInput::new(stdin)?),
            stdout: Box::new(stdout),
        })
    }
}

struct LinuxWorkerInput {
    pipe: ChildStdin,
    deadline: Instant,
}

impl LinuxWorkerInput {
    fn new(pipe: ChildStdin) -> io::Result<Self> {
        fcntl_setfl(&pipe, fcntl_getfl(&pipe)? | OFlags::NONBLOCK)?;
        Ok(Self {
            pipe,
            deadline: Instant::now(),
        })
    }
}

impl WorkerInput for LinuxWorkerInput {
    fn set_deadline(&mut self, deadline: Instant) {
        self.deadline = deadline;
    }
}

impl Write for LinuxWorkerInput {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        loop {
            let remaining = self.deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Worker input timed out",
                ));
            }
            match self.pipe.write(bytes) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                result => return result,
            }
            let timeout = Timespec {
                tv_sec: remaining.as_secs().try_into().unwrap_or(i64::MAX),
                tv_nsec: remaining.subsec_nanos().into(),
            };
            let mut fds = [PollFd::new(&self.pipe, PollFlags::OUT)];
            match poll(&mut fds, Some(&timeout)) {
                Ok(_) | Err(Errno::INTR) => {}
                Err(error) => return Err(error.into()),
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.pipe.flush()
    }
}

pub(crate) struct LinuxWorkerProcess {
    child: Child,
    exit: Option<WorkerExit>,
}

impl LinuxWorkerProcess {
    pub(crate) fn new(child: Child) -> Self {
        Self { child, exit: None }
    }

    fn group(&self) -> io::Result<Pid> {
        i32::try_from(self.child.id())
            .ok()
            .and_then(Pid::from_raw)
            .ok_or_else(|| io::Error::other("invalid Worker process group"))
    }

    fn exited(group: Pid) -> io::Result<bool> {
        // Leave the child waitable so it still owns the process group ID
        // when we signal any members that outlived it.
        loop {
            match waitid(
                WaitId::Pid(group),
                WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
            ) {
                Ok(status) => return Ok(status.is_some()),
                Err(Errno::INTR) => continue,
                Err(error) => return Err(error.into()),
            }
        }
    }

    fn stop_group_and_reap(&mut self, group: Pid, exited: bool) -> io::Result<()> {
        let group_result = match kill_process_group(group, Signal::KILL) {
            Ok(()) | Err(Errno::SRCH) => Ok(()),
            Err(error) => Err(io::Error::from(error)),
        };
        if !exited && let Err(error) = self.child.kill() {
            return group_result.and(Err(error));
        }
        let wait_result = self.child.wait().map(|status| {
            self.exit = Some(if status.success() {
                WorkerExit::Success
            } else {
                WorkerExit::Failure
            });
        });
        group_result.and(wait_result)
    }
}

impl ProcessController for LinuxWorkerProcess {
    fn id(&self) -> u32 {
        self.child.id()
    }

    fn try_wait(&mut self) -> io::Result<Option<WorkerExit>> {
        if self.exit.is_none() {
            let group = self.group()?;
            if Self::exited(group)? {
                self.stop_group_and_reap(group, true)?;
            }
        }
        Ok(self.exit)
    }

    fn terminate(&mut self) -> io::Result<()> {
        if self.exit.is_none() {
            let group = self.group()?;
            let exited = Self::exited(group)?;
            self.stop_group_and_reap(group, exited)?;
        }
        Ok(())
    }
}

impl Drop for LinuxWorkerProcess {
    fn drop(&mut self) {
        if let Err(error) = self.terminate() {
            eprintln!("mcbtodo-daemon: reaping Worker {}: {error}", self.id());
        }
    }
}
