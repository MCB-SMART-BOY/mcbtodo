//! IPC used by the CLI and terminal client. The protocol crate owns framing and peer checks.

use std::{io, path::Path};

use anyhow::{Context, Result, bail};
use chrono_tz::Tz;
use mcbtodo_domain::{CalendarItem, time::DateSpan};
use mcbtodo_platform::WorkspacePaths;
use mcbtodo_protocol::{ErrorCode, ProtocolError, Request, Response, TimerStatus, request};

pub(crate) fn timer(paths: &WorkspacePaths, operation: Request) -> Result<TimerStatus> {
    match exchange(paths, operation)? {
        Response::Status(status) => Ok(status),
        _ => bail!("unexpected response to timer request"),
    }
}

pub(crate) fn calendar(
    paths: &WorkspacePaths,
    window: DateSpan,
    timezone: Tz,
) -> Result<Vec<CalendarItem>> {
    match exchange(
        paths,
        Request::Calendar {
            start: window.start(),
            through: window.through(),
            timezone: timezone.name().into(),
        },
    )? {
        Response::Calendar { items } => Ok(items),
        _ => bail!("unexpected response to calendar request"),
    }
}

pub(crate) fn exchange(paths: &WorkspacePaths, operation: Request) -> Result<Response> {
    let socket = paths.socket_path();
    let exchange = request(&socket, operation);
    if let Err(ProtocolError::Io(error)) = &exchange
        && matches!(
            error.kind(),
            io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
        )
    {
        return exchange.context("Start mcbtodo-daemon and try again");
    }
    match exchange.with_context(|| format!("contacting mcbtodo-daemon at {}", socket.display()))? {
        Response::Error { code, message } => bail!("{code:?}: {message}"),
        response => Ok(response),
    }
}

pub(crate) enum WriteFailure {
    StaleRevision,
    Rejected(String),
    /// The daemon may have acted, but no recognized confirmation arrived.
    Unknown,
}

pub(crate) fn owns_vault(paths: &WorkspacePaths, root: &Path) -> Result<bool> {
    if cfg!(target_os = "linux") {
        paths.is_default_vault(root)
    } else {
        Ok(false)
    }
}

pub(crate) fn submit_write(
    paths: &WorkspacePaths,
    operation: Request,
    confirmation: impl FnOnce(Response) -> Option<bool>,
) -> Result<bool, WriteFailure> {
    match request(paths.socket_path(), operation) {
        Ok(Response::Error {
            code: ErrorCode::StaleRevision,
            ..
        }) => Err(WriteFailure::StaleRevision),
        Ok(Response::Error { message, .. }) => Err(WriteFailure::Rejected(message)),
        Ok(response) => confirmation(response).ok_or(WriteFailure::Unknown),
        Err(ProtocolError::Io(error))
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            Err(WriteFailure::Rejected(
                "Start mcbtodo-daemon and try again".into(),
            ))
        }
        Err(_) => Err(WriteFailure::Unknown),
    }
}
