use std::path::Path;

use crate::{ProtocolError, Request, Response};

#[cfg(target_os = "linux")]
use std::{
    io::{Error, ErrorKind, Read},
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

#[cfg(target_os = "linux")]
use mcbtodo_platform::local_transport::UnixSocketTransport;
#[cfg(target_os = "linux")]
use mcbtodo_ports::transport::{LocalPeer, LocalTransport};

/// Sends one request to `socket_path` and receives its complete typed response.
/// Remote errors arrive as [`Response::Error`]. Connection and framing errors
/// are returned as [`ProtocolError`]. On Linux the peer must run as the
/// calling user and the entire exchange has a three-second deadline.
///
/// ```no_run
/// use mcbtodo_protocol::{Request, Response, request};
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// match request("/path/to/state/mcbtodo-daemon.sock", Request::Status)? {
///     Response::Status(status) => println!("{:?}", status.state),
///     Response::Error { code, message } => eprintln!("{code:?}: {message}"),
///     _ => unreachable!("expected a timer response"),
/// }
/// # Ok(())
/// # }
/// ```
pub fn request(socket_path: impl AsRef<Path>, command: Request) -> Result<Response, ProtocolError> {
    #[cfg(target_os = "linux")]
    {
        request_with(&UnixSocketTransport, socket_path.as_ref(), command)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (socket_path, command);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "mcbtodo Daemon transport requires Linux",
        )
        .into())
    }
}

#[cfg(target_os = "linux")]
fn request_with(
    transport: &dyn LocalTransport,
    socket_path: &Path,
    command: Request,
) -> Result<Response, ProtocolError> {
    let cancelled = AtomicBool::new(false);
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut stream = transport.connect(socket_path, deadline, &cancelled)?;
    if stream.peer() != LocalPeer::SameUser {
        return Err(Error::new(
            ErrorKind::PermissionDenied,
            "Daemon peer must run as the calling user",
        )
        .into());
    }
    crate::wire::write(&mut stream, &command)?;
    let response = crate::wire::read(&mut stream)?;
    if matches!(command, Request::Outline { .. }) {
        read_outline(response, &mut stream)
    } else {
        Ok(response)
    }
}

#[cfg(target_os = "linux")]
fn read_outline(mut response: Response, stream: &mut impl Read) -> Result<Response, ProtocolError> {
    let mut items = Vec::new();
    loop {
        match response {
            Response::Outline { items: page, more } => {
                items.extend(page);
                if !more {
                    return Ok(Response::Outline { items, more: false });
                }
            }
            Response::Error { .. } => return Ok(response),
            _ => {
                return Err(
                    Error::new(ErrorKind::InvalidData, "unexpected outline response").into(),
                );
            }
        }
        response = crate::wire::read(stream)?;
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::io::{self, Write};

    use mcbtodo_ports::{Cancellation, transport::LocalConnection};

    use super::*;

    #[test]
    fn outline_requires_a_final_frame_before_publishing_rows() {
        let mut interrupted = io::empty();
        let first = Response::Outline {
            items: vec![mcbtodo_domain::OutlineItem {
                id: uuid::Uuid::nil(),
                depth: 0,
                title: "Project".into(),
                source_path: "projects/project/project.md".into(),
                project_id: Some(uuid::Uuid::nil()),
                task_id: None,
                blocked: false,
                revision: "revision".into(),
            }],
            more: true,
        };
        assert!(matches!(
            read_outline(first, &mut interrupted),
            Err(ProtocolError::Io(error)) if error.kind() == ErrorKind::UnexpectedEof
        ));
    }

    struct OtherUser;

    impl LocalTransport for OtherUser {
        fn connect<'a>(
            &self,
            _: &Path,
            _: Instant,
            _: &'a dyn Cancellation,
        ) -> io::Result<Box<dyn LocalConnection + 'a>> {
            Ok(Box::new(OtherUser))
        }
    }

    impl LocalConnection for OtherUser {
        fn peer(&self) -> LocalPeer {
            LocalPeer::DifferentUser
        }
    }

    impl Read for OtherUser {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            panic!("unauthorized peer must not receive an exchange");
        }
    }

    impl Write for OtherUser {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            panic!("unauthorized peer must not receive a request");
        }

        fn flush(&mut self) -> io::Result<()> {
            panic!("unauthorized peer must not receive a request");
        }
    }

    #[test]
    fn typed_client_rejects_different_user_before_request_io() {
        let error = request_with(&OtherUser, Path::new("unused"), Request::Status).unwrap_err();
        assert!(
            matches!(error, ProtocolError::Io(cause) if cause.kind() == ErrorKind::PermissionDenied)
        );
    }
}
