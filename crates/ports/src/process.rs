use std::{
    io::{self, Read, Write},
    path::Path,
    time::Instant,
};

/// Starts a Worker with isolated protocol stdin/stdout and a sanitized
/// environment. Reject platforms that lack the required supervision.
pub trait ProcessLauncher: Send {
    fn launch_worker(&self, executable: &Path, home: &Path) -> io::Result<LaunchedWorker>;
}

pub struct LaunchedWorker {
    pub process: Box<dyn ProcessController>,
    pub stdin: Box<dyn WorkerInput>,
    pub stdout: Box<dyn Read + Send>,
}

/// Worker input with a host-owned absolute deadline. Writes use the same
/// deadline until it is replaced and return `TimedOut` when it expires.
/// An error can follow partial delivery. The host must not resend the frame.
pub trait WorkerInput: Write + Send {
    fn set_deadline(&mut self, deadline: Instant);
}

/// Owns a single Worker. Dropping a live controller must attempt termination
/// and reaping. The host interprets the protocol and decides the run outcome.
pub trait ProcessController: Send {
    /// Process ID for local diagnostics.
    fn id(&self) -> u32;

    /// Polls for exit without waiting for a running Worker. An observed exit
    /// signals remaining members of the owned group before reaping the
    /// Worker. Subsequent probes return the cached exit status.
    fn try_wait(&mut self) -> io::Result<Option<WorkerExit>>;

    /// Terminate the owned process group and reap the Worker. Safe to call
    /// again after a preceding successful termination or observed exit.
    fn terminate(&mut self) -> io::Result<()>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkerExit {
    Success,
    Failure,
}
