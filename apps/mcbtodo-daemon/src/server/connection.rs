use std::{
    io::{self, Read, Write},
    os::unix::net::UnixStream,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};

use mcbtodo_platform::local_transport::UnixSocketConnection;
use mcbtodo_ports::transport::{LocalConnection, LocalPeer};
use mcbtodo_protocol::{ErrorCode, ProtocolError, Request, Response, wire};

const TRANSFER_TIMEOUT: Duration = Duration::from_secs(1);

/// One accepted connection. The I/O thread wakes the scheduler after reading a
/// complete request and closes the wake stream when the connection is finished.
pub(super) struct Connection {
    wake: UnixStream,
    request: Receiver<Request>,
    reply: SyncSender<Response>,
    cancelled: Arc<AtomicBool>,
}

pub(super) enum ConnectionEvent {
    Request(Request),
    Finished,
}

impl Connection {
    pub fn start(socket: UnixStream) -> io::Result<Self> {
        let read_deadline = Instant::now() + TRANSFER_TIMEOUT;
        let (wake, notifier) = UnixStream::pair()?;
        let (send_request, request) = mpsc::sync_channel(1);
        let (reply, receive_reply) = mpsc::sync_channel(1);
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancel = Arc::clone(&cancelled);
        thread::Builder::new()
            .name("mcbtodo-ipc".into())
            .spawn(move || {
                exchange(
                    socket,
                    notifier,
                    send_request,
                    receive_reply,
                    read_deadline,
                    &cancel,
                );
            })?;
        Ok(Self {
            wake,
            request,
            reply,
            cancelled,
        })
    }

    pub fn wake(&self) -> &UnixStream {
        &self.wake
    }

    /// Called only after poll reports that the wake stream is readable or closed.
    pub fn event(&mut self) -> io::Result<ConnectionEvent> {
        loop {
            match self.wake.read(&mut [0]) {
                Ok(0) => return Ok(ConnectionEvent::Finished),
                Ok(_) => {
                    return self
                        .request
                        .try_recv()
                        .map(ConnectionEvent::Request)
                        .map_err(|error| {
                            io::Error::other(format!("missing prepared IPC request: {error}"))
                        });
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
    }

    pub fn reply(&self, response: Response) {
        // The lane sends one request and therefore has room for its only reply.
        let _ = self.reply.try_send(response);
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

fn exchange(
    socket: UnixStream,
    mut notifier: UnixStream,
    send_request: SyncSender<Request>,
    receive_reply: Receiver<Response>,
    read_deadline: Instant,
    cancelled: &AtomicBool,
) {
    let mut stream = match UnixSocketConnection::from_accepted(socket, read_deadline, cancelled) {
        Ok(stream) if stream.peer() == LocalPeer::SameUser => stream,
        Ok(_) => return,
        Err(error) => {
            eprintln!("mcbtodo-daemon: preparing local connection: {error}");
            return;
        }
    };
    let response = match wire::read::<Request>(&mut stream) {
        Ok(request) => {
            if send_request.try_send(request).is_err() || notifier.write_all(&[1]).is_err() {
                return;
            }
            let Ok(response) = receive_reply.recv() else {
                return;
            };
            response
        }
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
    stream.set_deadline(Instant::now() + TRANSFER_TIMEOUT);
    let result = match response {
        Response::Outline { items, .. } => wire::write_outline(&mut stream, items),
        Response::Inbox { items, .. } => wire::write_inbox(&mut stream, items),
        // The encoder checks size before writing any bytes.
        response @ Response::Calendar { .. } => match wire::write(&mut stream, &response) {
            Err(ProtocolError::FrameSize(_)) => wire::write(
                &mut stream,
                &Response::Error {
                    code: ErrorCode::InvalidRequest,
                    message: "calendar result exceeds the IPC size limit; try a smaller date range"
                        .into(),
                },
            ),
            result => result,
        },
        response => wire::write(&mut stream, &response),
    };
    if let Err(error) = result {
        eprintln!("mcbtodo-daemon: sending response: {error}");
    }
}
