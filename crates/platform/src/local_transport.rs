//! Linux local byte streams with authenticated peer identity and bounded I/O.

use std::{
    io::{self, Read, Write},
    os::{fd::AsFd, unix::net::UnixStream},
    path::Path,
    time::{Duration, Instant},
};

use mcbtodo_ports::{
    Cancellation,
    transport::{LocalConnection, LocalPeer, LocalTransport},
};
use rustix::{
    event::{PollFd, PollFlags, Timespec, poll},
    io::Errno,
    net::{AddressFamily, SocketAddrUnix, SocketFlags, SocketType, connect, socket_with, sockopt},
    process::geteuid,
};

const CANCEL_POLL: Duration = Duration::from_millis(25);

struct Deadline<'a> {
    expires: Instant,
    cancel: &'a dyn Cancellation,
}

impl Deadline<'_> {
    fn remaining(&self) -> io::Result<Duration> {
        if self.cancel.is_cancelled() {
            return Err(io::Error::other("local transport cancelled"));
        }
        let left = self.expires.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "local transport timed out",
            ));
        }
        Ok(left)
    }

    fn wait(&self, socket: &impl AsFd, flags: PollFlags) -> io::Result<()> {
        let mut fds = [PollFd::new(socket, flags)];
        loop {
            let interval = self.remaining()?.min(CANCEL_POLL);
            let timeout = Timespec {
                tv_sec: 0,
                tv_nsec: interval.subsec_nanos().into(),
            };
            match poll(&mut fds, Some(&timeout)) {
                Ok(0) | Err(Errno::INTR) => {}
                Ok(_) => return Ok(()),
                Err(error) => return Err(error.into()),
            }
        }
    }
}

/// A Linux Unix stream with verified peer metadata and bounded transfers.
pub struct UnixSocketConnection<'a> {
    socket: UnixStream,
    deadline: Deadline<'a>,
    peer: LocalPeer,
}

/// Linux stream connector that authenticates peer credentials before I/O.
pub struct UnixSocketTransport;

impl<'a> UnixSocketConnection<'a> {
    /// Wrap a socket returned by a Linux listener. The caller authorizes the
    /// authenticated peer before reading or writing protocol data.
    pub fn from_accepted(
        socket: UnixStream,
        expires: Instant,
        cancel: &'a dyn Cancellation,
    ) -> io::Result<Self> {
        socket.set_nonblocking(true)?;
        Self::authenticate(socket, Deadline { expires, cancel })
    }

    fn authenticate(socket: UnixStream, deadline: Deadline<'a>) -> io::Result<Self> {
        let peer = if sockopt::socket_peercred(&socket)?.uid == geteuid() {
            LocalPeer::SameUser
        } else {
            LocalPeer::DifferentUser
        };
        deadline.remaining()?;
        Ok(Self {
            socket,
            deadline,
            peer,
        })
    }

    /// Start a new transfer phase with a host-provided absolute deadline.
    pub fn set_deadline(&mut self, expires: Instant) {
        self.deadline.expires = expires;
    }
}

impl LocalTransport for UnixSocketTransport {
    fn connect<'a>(
        &self,
        path: &Path,
        expires: Instant,
        cancel: &'a dyn Cancellation,
    ) -> io::Result<Box<dyn LocalConnection + 'a>> {
        let deadline = Deadline { expires, cancel };
        deadline.remaining()?;
        let addr = SocketAddrUnix::new(path)?;
        let socket = socket_with(
            AddressFamily::UNIX,
            SocketType::STREAM,
            SocketFlags::NONBLOCK | SocketFlags::CLOEXEC,
            None,
        )?;
        match connect(&socket, &addr) {
            Ok(()) => {}
            Err(Errno::INPROGRESS) => {
                deadline.wait(&socket, PollFlags::OUT)?;
                sockopt::socket_error(&socket)??;
            }
            Err(error) => return Err(error.into()),
        }
        Ok(Box::new(UnixSocketConnection::authenticate(
            UnixStream::from(socket),
            deadline,
        )?))
    }
}

impl LocalConnection for UnixSocketConnection<'_> {
    fn peer(&self) -> LocalPeer {
        self.peer
    }
}

impl UnixSocketConnection<'_> {
    fn transfer(
        &self,
        flags: PollFlags,
        mut operation: impl FnMut(&UnixStream) -> io::Result<usize>,
    ) -> io::Result<usize> {
        loop {
            self.deadline.remaining()?;
            match operation(&self.socket) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    self.deadline.wait(&self.socket, flags)?;
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                result => return result,
            }
        }
    }
}

impl Read for UnixSocketConnection<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.transfer(PollFlags::IN, |mut socket| socket.read(buffer))
    }
}

impl Write for UnixSocketConnection<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.transfer(PollFlags::OUT, |mut socket| socket.write(buffer))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn accepted_socket_obeys_identity_deadlines_and_cancellation() -> io::Result<()> {
        let (mut client, accepted) = UnixStream::pair()?;
        client.set_read_timeout(Some(Duration::from_secs(1)))?;
        let cancel = AtomicBool::new(false);
        let mut stream = UnixSocketConnection::from_accepted(
            accepted,
            Instant::now() + Duration::from_secs(1),
            &cancel,
        )?;
        assert_eq!(stream.peer(), LocalPeer::SameUser);

        let mut byte = [0];
        client.write_all(b"q")?;
        stream.set_deadline(Instant::now());
        assert_eq!(
            stream.read_exact(&mut byte).unwrap_err().kind(),
            io::ErrorKind::TimedOut,
        );
        stream.set_deadline(Instant::now() + Duration::from_secs(1));
        stream.read_exact(&mut byte)?;
        assert_eq!(&byte, b"q");
        stream.write_all(b"r")?;
        client.read_exact(&mut byte)?;
        assert_eq!(&byte, b"r");

        cancel.store(true, Ordering::Release);
        assert_eq!(
            stream.write_all(b"x").unwrap_err().to_string(),
            "local transport cancelled",
        );
        Ok(())
    }
}
