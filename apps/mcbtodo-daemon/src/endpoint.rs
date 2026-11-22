use std::fs::{self, File, Metadata, TryLockError};
use std::io;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use mcbtodo_platform::private_file::open_private_file;
use rustix::io::Errno;
use rustix::net::{AddressFamily, SocketAddrUnix, SocketFlags, SocketType, connect, socket_with};
use rustix::process::geteuid;

pub struct Endpoint {
    pub listener: UnixListener,
    _socket: BoundSocket,
    // Keep the lock until after the listener and its pathname are dropped.
    _lock: StateLock,
}

struct StateLock(File);

impl StateLock {
    fn acquire(file: File) -> Result<Self> {
        const STARTUP_GRACE: Duration = Duration::from_millis(250);
        const RETRY_INTERVAL: Duration = Duration::from_millis(10);
        let deadline = Instant::now() + STARTUP_GRACE;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Self(file)),
                Err(TryLockError::WouldBlock) => {
                    // After a forced stop, a concurrent fork may still hold a
                    // copy until exec closes it. Keep using the same inode.
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    ensure!(
                        !remaining.is_zero(),
                        "the state directory lock is held by another mcbtodo-daemon or backup"
                    );
                    thread::sleep(RETRY_INTERVAL.min(remaining));
                }
                Err(TryLockError::Error(error)) => {
                    return Err(error).context("locking daemon state");
                }
            }
        }
    }
}

impl Drop for StateLock {
    fn drop(&mut self) {
        // A concurrent fork can retain the open file description until exec.
        // Release ownership explicitly rather than waiting for every copy to close.
        if let Err(error) = self.0.unlock() {
            eprintln!("mcbtodo-daemon: releasing the state directory lock: {error}");
        }
    }
}

impl Endpoint {
    pub fn bind(state_dir: &Path, socket_path: &Path, lock_path: &Path) -> Result<Self> {
        let state = fs::symlink_metadata(state_dir).with_context(|| {
            format!(
                "opening state directory {} (run `mcbtodo init` first)",
                state_dir.display()
            )
        })?;
        ensure!(
            state.is_dir() && owned_by_us(&state) && state.mode() & 0o077 == 0,
            "state directory {} must be a private directory owned by the current user",
            state_dir.display()
        );
        ensure!(
            socket_path.parent() == Some(state_dir),
            "socket must be directly inside state directory"
        );
        ensure!(
            lock_path.parent() == Some(state_dir),
            "daemon lock must be directly inside state directory"
        );
        // Keep the lock file on disk so competing processes always lock the same inode.
        let lock = StateLock::acquire(open_private_file(lock_path)?)?;

        remove_stale_socket(socket_path)?;
        let listener = UnixListener::bind(socket_path)
            .with_context(|| format!("binding {}", socket_path.display()))?;
        let socket = BoundSocket {
            path: socket_path.to_owned(),
            identity: identity(&fs::symlink_metadata(socket_path)?),
        };
        fs::set_permissions(socket_path, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("setting permissions on {}", socket_path.display()))?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener,
            _socket: socket,
            _lock: lock,
        })
    }
}

fn remove_stale_socket(path: &Path) -> Result<()> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).with_context(|| format!("checking {}", path.display())),
    };
    ensure!(
        meta.file_type().is_socket() && owned_by_us(&meta),
        "refusing to replace {}: expected an owned Unix socket",
        path.display()
    );
    let probe = socket_with(
        AddressFamily::UNIX,
        SocketType::STREAM,
        SocketFlags::CLOEXEC | SocketFlags::NONBLOCK,
        None,
    )?;
    // A full backlog or any indeterminate result must leave the endpoint alone.
    match connect(&probe, &SocketAddrUnix::new(path)?) {
        Err(Errno::CONNREFUSED) => (),
        Ok(()) => bail!("a listener is already active at {}", path.display()),
        Err(error) => return Err(error).with_context(|| format!("probing {}", path.display())),
    }
    let current = fs::symlink_metadata(path)?;
    ensure!(
        identity(&meta) == identity(&current) && current.file_type().is_socket(),
        "socket at {} changed during startup",
        path.display()
    );
    fs::remove_file(path).with_context(|| format!("removing stale socket {}", path.display()))
}

struct BoundSocket {
    path: PathBuf,
    identity: (u64, u64),
}

impl Drop for BoundSocket {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.path)
            .is_ok_and(|meta| meta.file_type().is_socket() && identity(&meta) == self.identity)
            && let Err(error) = fs::remove_file(&self.path)
        {
            eprintln!("mcbtodo-daemon: removing {}: {error}", self.path.display());
        }
    }
}

fn owned_by_us(meta: &Metadata) -> bool {
    meta.uid() == geteuid().as_raw()
}

fn identity(meta: &Metadata) -> (u64, u64) {
    (meta.dev(), meta.ino())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_startup_can_wait_for_a_closing_lock_descriptor() -> Result<()> {
        let dir = tempfile::Builder::new()
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir()?;
        let socket = dir.path().join("s");
        let lock = dir.path().join("daemon.lock");
        let previous = open_private_file(&lock)?;
        previous.try_lock()?;
        let inherited = previous.try_clone()?;
        drop(previous);

        thread::scope(|scope| {
            scope.spawn(move || {
                thread::sleep(Duration::from_millis(50));
                drop(inherited);
            });
            Endpoint::bind(dir.path(), &socket, &lock)
        })?;
        Ok(())
    }

    #[test]
    fn dropping_endpoint_releases_lock_even_with_a_duplicate_descriptor() -> Result<()> {
        let dir = tempfile::Builder::new()
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir()?;
        let socket = dir.path().join("s");
        let lock = dir.path().join("daemon.lock");
        let endpoint = Endpoint::bind(dir.path(), &socket, &lock)?;
        let duplicate = endpoint._lock.0.try_clone()?;

        let socket_identity = identity(&fs::symlink_metadata(&socket)?);
        assert!(Endpoint::bind(dir.path(), &socket, &lock).is_err());
        assert_eq!(identity(&fs::symlink_metadata(&socket)?), socket_identity);
        assert!(matches!(
            endpoint.listener.accept(),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock
        ));
        drop(endpoint);
        let replacement = Endpoint::bind(dir.path(), &socket, &lock)?;
        drop(duplicate);
        drop(replacement);
        Ok(())
    }
}
