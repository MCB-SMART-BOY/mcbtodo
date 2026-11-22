//! Relay a single model exchange inside an isolated launcher.

use std::{
    io::{self, Read, Write},
    net::{Shutdown, TcpStream},
    os::unix::net::UnixStream,
    thread,
    time::Instant,
};

const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(10);

const MAX_MODEL_REQUEST: usize = 256 * 1024;
const MAX_MODEL_RESPONSE: usize = 2 * 1024 * 1024;

/// Forward a single private loopback connection to an already authorized
/// model socket. The launcher must isolate the network and verify the socket.
/// This relay enforces byte budgets and propagates half-closes. The model
/// service owns HTTP validation, credential use and its upstream policy.
pub(crate) fn relay_model_exchange(
    loopback: &TcpStream,
    model: &UnixStream,
    deadline: Instant,
) -> io::Result<()> {
    if !loopback.peer_addr()?.ip().is_loopback() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "model relay requires a loopback peer",
        ));
    }
    loopback.set_nonblocking(true)?;
    model.set_nonblocking(true)?;
    let abort = || {
        let _ = loopback.shutdown(Shutdown::Both);
        let _ = model.shutdown(Shutdown::Both);
    };
    thread::scope(|scope| {
        let request = thread::Builder::new()
            .name("mcbtodo-codex-model-request".into())
            .spawn_scoped(scope, || {
                let result = transfer(loopback, model, MAX_MODEL_REQUEST, deadline)
                    .and_then(|()| model.shutdown(Shutdown::Write));
                if result.is_err() {
                    abort();
                }
                result
            })?;
        let response = transfer(model, loopback, MAX_MODEL_RESPONSE, deadline)
            .and_then(|()| loopback.shutdown(Shutdown::Write));
        if response.is_err() {
            abort();
        }
        request
            .join()
            .map_err(|_| io::Error::other("model relay worker terminated"))?
            .and(response)
    })
}

fn transfer(
    mut source: impl Read,
    mut destination: impl Write,
    mut remaining: usize,
    deadline: Instant,
) -> io::Result<()> {
    let mut buffer = [0; 8192];
    loop {
        check_deadline(deadline)?;
        let capacity = buffer.len().min(remaining.saturating_add(1));
        match source.read(&mut buffer[..capacity]) {
            Ok(0) => return Ok(()),
            Ok(count) if count > remaining => {
                return Err(io::Error::other("model relay byte limit exceeded"));
            }
            Ok(count) => {
                remaining -= count;
                let mut pending = &buffer[..count];
                while !pending.is_empty() {
                    check_deadline(deadline)?;
                    match destination.write(pending) {
                        Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                        Ok(written) => pending = &pending[written..],
                        Err(error) => wait_for_io(error, deadline)?,
                    }
                }
            }
            Err(error) => wait_for_io(error, deadline)?,
        }
    }
}

fn check_deadline(deadline: Instant) -> io::Result<()> {
    if Instant::now() >= deadline {
        return Err(io::ErrorKind::TimedOut.into());
    }
    Ok(())
}

fn wait_for_io(error: io::Error, deadline: Instant) -> io::Result<()> {
    match error.kind() {
        io::ErrorKind::Interrupted => Ok(()),
        io::ErrorKind::WouldBlock => {
            thread::sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())));
            Ok(())
        }
        _ => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{net::TcpListener, time::Duration};

    #[test]
    fn streams_in_both_directions_and_propagates_half_close() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (loopback, _) = listener.accept().unwrap();
        let (model, mut backend) = UnixStream::pair().unwrap();
        let timeout = Duration::from_secs(2);
        client.set_read_timeout(Some(timeout)).unwrap();
        backend.set_read_timeout(Some(timeout)).unwrap();
        thread::scope(|scope| {
            let relay =
                scope.spawn(|| relay_model_exchange(&loopback, &model, Instant::now() + timeout));
            client.write_all(b"first").unwrap();
            let mut chunk = [0; 5];
            backend.read_exact(&mut chunk).unwrap();
            assert_eq!(&chunk, b"first");
            backend.write_all(b"delta").unwrap();
            client.read_exact(&mut chunk).unwrap();
            assert_eq!(&chunk, b"delta");
            client.shutdown(Shutdown::Write).unwrap();
            assert_eq!(backend.read(&mut chunk).unwrap(), 0);
            backend.write_all(b"final").unwrap();
            backend.shutdown(Shutdown::Write).unwrap();
            client.read_exact(&mut chunk).unwrap();
            assert_eq!(&chunk, b"final");
            assert_eq!(client.read(&mut chunk).unwrap(), 0);
            relay.join().unwrap().unwrap();
        });
    }

    #[test]
    fn rejects_over_limit_without_forwarding_excess_bytes() {
        let bytes = vec![1; 9_000];
        let mut received = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(2);
        transfer(bytes.as_slice(), &mut received, bytes.len(), deadline).unwrap();
        assert_eq!(received, bytes);
        received.clear();
        let error = transfer(bytes.as_slice(), &mut received, 8192, deadline).unwrap_err();
        assert_eq!(error.to_string(), "model relay byte limit exceeded");
        assert_eq!(received, &bytes[..8192]);

        let error = transfer(bytes.as_slice(), &mut received, 8192, Instant::now()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }
}
