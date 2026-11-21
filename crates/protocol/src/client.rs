use std::path::Path;

use crate::{ProtocolError, Request, Response};

/// Sends one request to `socket_path` and receives its typed response.
/// Remote errors arrive as [`Response::Error`]. Connection and framing errors
/// are returned as [`ProtocolError`].
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
    #[cfg(unix)]
    {
        let mut stream = std::os::unix::net::UnixStream::connect(socket_path)?;
        let timeout = Some(std::time::Duration::from_secs(3));
        stream.set_read_timeout(timeout)?;
        stream.set_write_timeout(timeout)?;
        crate::wire::write(&mut stream, &command)?;
        crate::wire::read(&mut stream)
    }
    #[cfg(not(unix))]
    {
        let _ = (socket_path, command);
        Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "Unix sockets required").into())
    }
}
