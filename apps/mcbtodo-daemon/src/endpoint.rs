use std::fs::{self, File, Metadata, TryLockError};
use std::io;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use rustix::fs::{Mode, OFlags};
use rustix::io::Errno;
use rustix::net::{AddressFamily, SocketAddrUnix, SocketFlags, SocketType, connect, socket_with};
use rustix::process::geteuid;

pub struct Endpoint {
    pub listener: UnixListener,
    _socket: BoundSocket,
    // Keep the lock until after the listener and its pathname are dropped.
    _lock: File,
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
        let lock = open_private_file(lock_path)?;
        match lock.try_lock() {
            Ok(()) => (),
            Err(TryLockError::WouldBlock) => {
                bail!("mcbtodo-daemon already holds the state directory lock")
            }
            Err(TryLockError::Error(error)) => return Err(error).context("locking daemon state"),
        }

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

pub fn open_private_file(path: &Path) -> Result<File> {
    let file = File::from(
        rustix::fs::open(
            path,
            OFlags::RDWR | OFlags::CREATE | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
            Mode::RUSR | Mode::WUSR,
        )
        .with_context(|| format!("opening {}", path.display()))?,
    );
    let meta = file.metadata()?;
    ensure!(
        meta.is_file() && owned_by_us(&meta) && meta.mode() & 0o077 == 0 && meta.nlink() == 1,
        "{} must be a private regular file owned by the current user",
        path.display()
    );
    Ok(file)
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
