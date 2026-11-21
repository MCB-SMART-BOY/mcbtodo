//! Home-relative paths and application configuration for mcbtodo.

pub mod agent_policy;
pub mod process;

use std::{
    env, fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};
#[cfg(unix)]
use std::{
    ffi::OsStr,
    os::fd::OwnedFd,
    sync::atomic::{AtomicU64, Ordering},
};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};

use anyhow::{Context, Result, ensure};
use chrono_tz::Tz;
#[cfg(unix)]
use rustix::process::geteuid;
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

const MAX_USER_CONFIG_BYTES: usize = 1024 * 1024;
const MAX_USER_STATE_BYTES: usize = 4096;
#[cfg(unix)]
static CACHE_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Application-wide calendar settings.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    /// IANA time zone used for calendar operations.
    pub timezone: Tz,
    /// Initial TUI view, either the calendar month or the last saved view.
    #[serde(default)]
    pub home_view: HomeViewPolicy,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HomeViewPolicy {
    #[default]
    Month,
    RestoreLastView,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            timezone: chrono_tz::UTC,
            home_view: HomeViewPolicy::Month,
        }
    }
}

/// The fixed mcbtodo layout beneath a user's home directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspacePaths {
    home: PathBuf,
}

impl WorkspacePaths {
    /// Resolve paths using the current user's home directory.
    pub fn discover() -> Result<Self> {
        let home = env::home_dir()
            .filter(|path| path.is_absolute())
            .context("could not determine an absolute home directory for mcbtodo")?;
        Ok(Self::from_home(home))
    }

    /// Resolve paths relative to an explicit home without accessing the filesystem.
    pub fn from_home(home: PathBuf) -> Self {
        Self { home }
    }

    pub fn home_dir(&self) -> &Path {
        &self.home
    }

    fn config_dir(&self) -> PathBuf {
        self.home.join(".config").join("mcbtodo")
    }

    pub fn data_dir(&self) -> PathBuf {
        self.home.join(".mcbtodo")
    }

    pub fn config_path(&self) -> PathBuf {
        self.config_dir().join("config.toml")
    }

    pub fn keymap_path(&self) -> PathBuf {
        self.config_dir().join("keymap.toml")
    }

    pub fn agent_policy_path(&self) -> PathBuf {
        self.config_dir().join("agents.toml")
    }

    pub fn state_dir(&self) -> PathBuf {
        self.data_dir().join("state")
    }

    pub fn last_view_path(&self) -> PathBuf {
        self.state_dir().join("last-view")
    }

    pub fn focus_database_path(&self) -> PathBuf {
        self.state_dir().join("focus.sqlite3")
    }

    pub fn workflow_database_path(&self) -> PathBuf {
        self.state_dir().join("workflow.sqlite3")
    }

    pub fn daemon_lock_path(&self) -> PathBuf {
        self.state_dir().join("mcbtodo-daemon.lock")
    }

    pub fn proposals_dir(&self) -> PathBuf {
        self.state_dir().join("proposals")
    }

    pub fn vault_root(&self) -> PathBuf {
        self.data_dir().join("vault")
    }

    pub fn socket_path(&self) -> PathBuf {
        self.state_dir().join("mcbtodo-daemon.sock")
    }

    pub fn cache_dir(&self) -> PathBuf {
        self.data_dir().join("cache")
    }

    /// Read an existing config containing a required `timezone` and no other keys.
    pub fn load_config(&self) -> Result<AppConfig> {
        let path = self.config_path();
        let contents = self.read_config_file(&path)?;
        toml::from_str(&contents).with_context(|| format!("parsing {}", path.display()))
    }

    /// Read a direct child of the user config directory after validating its directories.
    ///
    /// The `.config` directory may be readable by other users, but not writable
    /// by them. The application config directory must be private; its files must
    /// be current-user-owned and not writable by group or other users.
    pub fn read_config_file(&self, path: &Path) -> Result<String> {
        let config_dir = self.config_dir();
        ensure!(
            path.parent() == Some(config_dir.as_path()),
            "{} is not a direct child of the mcbtodo config directory",
            path.display()
        );
        if let Err(error) = fs::symlink_metadata(&config_dir) {
            if error.kind() == io::ErrorKind::NotFound {
                return read_regular_file_checked(path, |metadata| {
                    ensure!(
                        owned_by_current_user(metadata),
                        "{} must be owned by the current user",
                        path.display()
                    );
                    Ok(())
                })
                .with_context(|| format!("reading {}", path.display()));
            }
            return Err(error).with_context(|| format!("checking {}", config_dir.display()));
        }
        validate_user_config_parent(&self.home.join(".config"))?;
        validate_private_dir(&config_dir)?;
        read_regular_file_checked(path, |metadata| {
            ensure!(
                owned_by_current_user(metadata),
                "{} must be owned by the current user",
                path.display()
            );
            #[cfg(unix)]
            ensure!(
                metadata.permissions().mode() & 0o022 == 0,
                "{} must not be writable by group or other users",
                path.display()
            );
            Ok(())
        })
        .with_context(|| format!("reading {}", path.display()))
    }

    /// Read a bounded private state file directly beneath the application state directory.
    pub fn read_state_file(&self, path: &Path) -> Result<String> {
        ensure!(
            path.parent() == Some(self.state_dir().as_path()),
            "{} is not a direct child of the mcbtodo state directory",
            path.display()
        );
        validate_private_dir(&self.state_dir())?;
        read_regular_file_checked(path, |metadata| {
            ensure!(
                owned_by_current_user(metadata),
                "{} must be owned by the current user",
                path.display()
            );
            #[cfg(unix)]
            ensure!(
                metadata.permissions().mode() & 0o077 == 0,
                "{} must deny group and other access",
                path.display()
            );
            Ok(())
        })
        .with_context(|| format!("reading {}", path.display()))
        .and_then(|contents| {
            ensure!(
                contents.len() <= MAX_USER_STATE_BYTES,
                "{} exceeds the {} byte state limit",
                path.display(),
                MAX_USER_STATE_BYTES
            );
            Ok(contents)
        })
    }

    /// Atomically replace a bounded private state file directly beneath the state directory.
    pub fn write_state_file(&self, path: &Path, contents: &[u8]) -> Result<()> {
        ensure!(
            path.parent() == Some(self.state_dir().as_path()),
            "{} is not a direct child of the mcbtodo state directory",
            path.display()
        );
        ensure!(
            contents.len() <= MAX_USER_STATE_BYTES,
            "{} exceeds the {} byte state limit",
            path.display(),
            MAX_USER_STATE_BYTES
        );
        ensure_private_dir(&self.data_dir())?;
        ensure_private_dir(&self.state_dir())?;
        let mut temporary = NamedTempFile::new_in(self.state_dir())
            .with_context(|| format!("staging {}", path.display()))?;
        temporary
            .write_all(contents)
            .and_then(|()| temporary.as_file().sync_all())
            .with_context(|| format!("writing {}", path.display()))?;
        temporary
            .persist(path)
            .with_context(|| format!("publishing {}", path.display()))?;
        Ok(())
    }

    /// Create private config, state and Vault directories and install the default config atomically.
    ///
    /// An existing config is preserved. On Unix, existing application directories must
    /// deny group and other access. Application directories cannot be symlinks.
    pub fn initialize(&self) -> Result<()> {
        ensure_private_dir(&self.data_dir())?;
        let config_parent = self.home.join(".config");
        ensure_user_config_parent(&config_parent)?;
        let config_dir = self.config_dir();
        ensure_private_dir(&config_dir)?;
        ensure_private_dir(&self.state_dir())?;
        ensure_private_dir(&self.vault_root())?;

        let path = self.config_path();
        if path.exists() {
            self.read_config_file(&path)?;
            return Ok(());
        }

        let contents = toml::to_string(&AppConfig::default()).context("serializing app config")?;
        let mut temporary = NamedTempFile::new_in(&config_dir)
            .with_context(|| format!("staging {}", path.display()))?;
        temporary
            .write_all(contents.as_bytes())
            .and_then(|()| temporary.as_file().sync_all())
            .with_context(|| format!("writing default config for {}", path.display()))?;

        #[cfg(unix)]
        let publish_result = {
            let directory = rustix::fs::open(
                config_dir,
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::CLOEXEC
                    | rustix::fs::OFlags::DIRECTORY
                    | rustix::fs::OFlags::NOFOLLOW,
                rustix::fs::Mode::empty(),
            )
            .map_err(io::Error::from)?;
            rustix::fs::renameat_with(
                &directory,
                temporary
                    .path()
                    .file_name()
                    .context("temporary config has no name")?,
                &directory,
                "config.toml",
                rustix::fs::RenameFlags::NOREPLACE,
            )
            .map_err(io::Error::from)
        };
        #[cfg(not(unix))]
        let publish_result = fs::hard_link(temporary.path(), &path);

        match publish_result {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                self.read_config_file(&path).map(|_| ())
            }
            Err(error) => Err(error).with_context(|| format!("installing {}", path.display())),
        }
    }

    /// Create or verify the private workspace and cache directories and return the cache path.
    pub fn prepare_cache(&self) -> Result<PathBuf> {
        #[cfg(unix)]
        {
            self.open_cache_directory(Path::new(""), true)?;
            Ok(self.cache_dir())
        }
        #[cfg(not(unix))]
        {
            ensure_private_dir(&self.data_dir())?;
            let cache = self.cache_dir();
            ensure_private_dir(&cache)?;
            Ok(cache)
        }
    }

    /// Create or verify a private cache subdirectory using normal relative components only.
    pub fn prepare_cache_directory(&self, relative: &Path) -> Result<PathBuf> {
        ensure!(
            relative.as_os_str().is_empty()
                || relative
                    .components()
                    .all(|component| matches!(component, Component::Normal(_))),
            "cache directory path must contain only normal relative components"
        );
        #[cfg(unix)]
        {
            self.open_cache_directory(relative, true)?;
            Ok(relative
                .components()
                .fold(self.cache_dir(), |mut path, component| {
                    let Component::Normal(name) = component else {
                        unreachable!("cache path components were validated")
                    };
                    path.push(name);
                    path
                }))
        }
        #[cfg(not(unix))]
        {
            let mut directory = self.prepare_cache()?;
            for component in relative.components() {
                let Component::Normal(name) = component else {
                    unreachable!("cache path components were validated")
                };
                directory.push(name);
                ensure_private_dir(&directory)?;
            }
            Ok(directory)
        }
    }

    /// Read a private regular file beneath the cache without following its final symlink.
    pub fn read_cache_file(&self, relative: &Path) -> Result<Vec<u8>> {
        validate_cache_relative_path(relative)?;
        #[cfg(unix)]
        {
            let name = relative.file_name().context("cache file path is empty")?;
            let parent = relative.parent().unwrap_or_else(|| Path::new(""));
            let directory = self.open_cache_directory(parent, true)?;
            let fd = rustix::fs::openat(
                &directory,
                name,
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::CLOEXEC
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::NONBLOCK,
                rustix::fs::Mode::empty(),
            )
            .map_err(io::Error::from)
            .with_context(|| format!("opening cache file {}", relative.display()))?;
            let file = fs::File::from(fd);
            validate_cache_file_stat(
                &rustix::fs::fstat(&file).map_err(io::Error::from)?,
                relative,
            )?;
            let mut contents = Vec::new();
            file.take(MAX_USER_CONFIG_BYTES as u64 + 1)
                .read_to_end(&mut contents)
                .with_context(|| format!("reading cache file {}", relative.display()))?;
            ensure!(
                contents.len() <= MAX_USER_CONFIG_BYTES,
                "cache file {} exceeds the {} byte limit",
                relative.display(),
                MAX_USER_CONFIG_BYTES
            );
            Ok(contents)
        }
        #[cfg(not(unix))]
        {
            let name = relative.file_name().context("cache file path is empty")?;
            let parent = relative.parent().unwrap_or_else(|| Path::new(""));
            let directory = self.prepare_cache_directory(parent)?;
            let path = directory.join(name);
            read_regular_file_checked(&path, |metadata| {
                ensure!(
                    owned_by_current_user(metadata),
                    "{} must be owned by the current user",
                    path.display()
                );
                #[cfg(unix)]
                ensure!(
                    metadata.permissions().mode() & 0o077 == 0,
                    "{} must deny group and other access",
                    path.display()
                );
                Ok(())
            })
            .with_context(|| format!("reading cache file {}", path.display()))
            .map(String::into_bytes)
        }
    }

    /// Atomically write a private regular file beneath a verified cache directory.
    pub fn write_cache_file(&self, relative: &Path, contents: &[u8]) -> Result<()> {
        validate_cache_relative_path(relative)?;
        ensure!(
            contents.len() <= MAX_USER_CONFIG_BYTES,
            "cache file exceeds the {} byte limit",
            MAX_USER_CONFIG_BYTES
        );
        ensure!(
            relative.file_name().is_some(),
            "cache file path must include a filename"
        );
        #[cfg(unix)]
        {
            let parent = relative.parent().unwrap_or_else(|| Path::new(""));
            let directory = self.open_cache_directory(parent, true)?;
            let destination = relative.file_name().unwrap();
            validate_existing_cache_file(&directory, destination, relative)?;
            let (temporary_name, temporary_fd) = create_cache_temporary(&directory, relative)?;
            let mut temporary = fs::File::from(temporary_fd);
            let result = (|| -> Result<()> {
                temporary
                    .write_all(contents)
                    .and_then(|()| temporary.sync_all())
                    .with_context(|| format!("writing cache file {}", relative.display()))?;
                rustix::fs::renameat(&directory, &temporary_name, &directory, destination)
                    .map_err(io::Error::from)
                    .with_context(|| format!("publishing cache file {}", relative.display()))?;
                Ok(())
            })();
            if result.is_err() {
                let _ =
                    rustix::fs::unlinkat(&directory, &temporary_name, rustix::fs::AtFlags::empty());
            }
            result
        }
        #[cfg(not(unix))]
        {
            let parent = relative.parent().unwrap_or_else(|| Path::new(""));
            let directory = self.prepare_cache_directory(parent)?;
            let destination = directory.join(relative.file_name().unwrap());
            match fs::symlink_metadata(&destination) {
                Ok(metadata) => {
                    ensure!(
                        metadata.is_file() && owned_by_current_user(&metadata),
                        "{} must be a regular file owned by the current user",
                        destination.display()
                    );
                    #[cfg(unix)]
                    {
                        ensure!(
                            metadata.nlink() == 1,
                            "{} must not have additional hard links",
                            destination.display()
                        );
                        ensure!(
                            metadata.permissions().mode() & 0o077 == 0,
                            "{} must deny group and other access",
                            destination.display()
                        );
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("checking cache file {}", destination.display()));
                }
            }
            let mut temporary = NamedTempFile::new_in(&directory)
                .with_context(|| format!("staging cache file {}", destination.display()))?;
            temporary
                .write_all(contents)
                .and_then(|()| temporary.as_file().sync_all())
                .with_context(|| format!("writing cache file {}", destination.display()))?;
            temporary
                .persist(&destination)
                .with_context(|| format!("publishing cache file {}", destination.display()))?;
            Ok(())
        }
    }

    #[cfg(unix)]
    fn open_cache_directory(&self, relative: &Path, create: bool) -> Result<OwnedFd> {
        use rustix::fs::{Mode, OFlags};

        let home = rustix::fs::open(
            &self.home,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::DIRECTORY | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map_err(io::Error::from)
        .with_context(|| format!("opening home directory {}", self.home.display()))?;
        let data = open_or_create_private_directory(&home, OsStr::new(".mcbtodo"), create)?;
        let cache = open_or_create_private_directory(&data, OsStr::new("cache"), create)?;
        let mut directory = cache;
        for component in relative.components() {
            let Component::Normal(name) = component else {
                unreachable!("cache path components were validated")
            };
            directory = open_or_create_private_directory(&directory, name, create)?;
        }
        Ok(directory)
    }
}

use std::path::Component;

#[cfg(unix)]
fn open_or_create_private_directory(
    parent: &OwnedFd,
    name: &OsStr,
    create: bool,
) -> Result<OwnedFd> {
    use rustix::fs::{Mode, OFlags, mkdirat, openat};

    let flags = OFlags::RDONLY | OFlags::CLOEXEC | OFlags::DIRECTORY | OFlags::NOFOLLOW;
    let fd = match openat(parent, name, flags, Mode::empty()) {
        Ok(fd) => fd,
        Err(error) if error == rustix::io::Errno::NOENT && create => {
            match mkdirat(parent, name, Mode::from_raw_mode(0o700)) {
                Ok(()) => {}
                Err(error) if error == rustix::io::Errno::EXIST => {}
                Err(error) => return Err(io::Error::from(error).into()),
            }
            openat(parent, name, flags, Mode::empty()).map_err(io::Error::from)?
        }
        Err(error) => return Err(io::Error::from(error).into()),
    };
    let stat = rustix::fs::fstat(&fd).map_err(io::Error::from)?;
    let mode = stat.st_mode;
    ensure!(
        mode & 0o170000 == 0o040000,
        "cache ancestor must be a directory"
    );
    ensure!(
        stat.st_uid == rustix::process::geteuid().as_raw(),
        "cache ancestor must be owned by the current user"
    );
    ensure!(
        mode & 0o077 == 0,
        "cache ancestor must deny group and other access"
    );
    Ok(fd)
}

#[cfg(unix)]
fn validate_cache_file_stat(stat: &rustix::fs::Stat, path: &Path) -> Result<()> {
    let mode = stat.st_mode;
    ensure!(
        mode & 0o170000 == 0o100000,
        "cache file {} must be a regular file",
        path.display()
    );
    ensure!(
        stat.st_uid == rustix::process::geteuid().as_raw(),
        "cache file {} must be owned by the current user",
        path.display()
    );
    ensure!(
        stat.st_nlink == 1,
        "cache file {} must have one link",
        path.display()
    );
    ensure!(
        mode & 0o077 == 0,
        "cache file {} must deny group and other access",
        path.display()
    );
    Ok(())
}

#[cfg(unix)]
fn validate_existing_cache_file(directory: &OwnedFd, name: &OsStr, path: &Path) -> Result<()> {
    use rustix::fs::{OFlags, openat};

    match openat(
        directory,
        name,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        rustix::fs::Mode::empty(),
    ) {
        Ok(fd) => validate_cache_file_stat(&rustix::fs::fstat(&fd).map_err(io::Error::from)?, path),
        Err(error) if error == rustix::io::Errno::NOENT => Ok(()),
        Err(error) => Err(io::Error::from(error).into()),
    }
}

#[cfg(unix)]
fn create_cache_temporary(directory: &OwnedFd, path: &Path) -> Result<(String, OwnedFd)> {
    use rustix::fs::{Mode, OFlags, openat};

    for _ in 0..128 {
        let sequence = CACHE_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let name = format!(".mcbtodo-cache-{}-{sequence}", std::process::id());
        match openat(
            directory,
            name.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::from_raw_mode(0o600),
        ) {
            Ok(fd) => return Ok((name, fd)),
            Err(error) if error == rustix::io::Errno::EXIST => continue,
            Err(error) => {
                return Err(io::Error::from(error))
                    .with_context(|| format!("staging cache file {}", path.display()));
            }
        }
    }
    anyhow::bail!(
        "could not allocate a temporary cache file for {}",
        path.display()
    )
}

fn validate_cache_relative_path(path: &Path) -> Result<()> {
    ensure!(
        !path.as_os_str().is_empty()
            && path
                .components()
                .all(|component| matches!(component, Component::Normal(_))),
        "cache path must contain only normal relative components"
    );
    Ok(())
}

#[cfg(all(test, unix))]
mod cache_directory_fd_tests {
    use super::*;

    #[test]
    fn cache_publication_stays_in_the_open_directory_after_path_replacement() -> Result<()> {
        use std::os::unix::fs::symlink;

        let home = tempfile::tempdir()?;
        let outside = tempfile::tempdir()?;
        let paths = WorkspacePaths::from_home(home.path().to_path_buf());
        let original = paths.prepare_cache_directory(Path::new("agents"))?;
        let opened = paths.open_cache_directory(Path::new("agents"), false)?;

        let moved = paths.cache_dir().join("agents-moved");
        fs::rename(&original, &moved)?;
        symlink(outside.path(), &original)?;

        let (temporary_name, temporary_fd) =
            create_cache_temporary(&opened, Path::new("agents/context.md"))?;
        let mut temporary = fs::File::from(temporary_fd);
        temporary.write_all(b"confined")?;
        temporary.sync_all()?;
        rustix::fs::renameat(&opened, temporary_name.as_str(), &opened, "context.md")
            .map_err(io::Error::from)?;

        assert_eq!(fs::read(moved.join("context.md"))?, b"confined");
        assert_eq!(fs::read_dir(outside.path())?.count(), 0);
        Ok(())
    }
}

/// Read a bounded regular file without following a final-component symlink on Unix.
pub fn read_regular_file(path: &Path) -> Result<String> {
    read_regular_file_checked(path, |_| Ok(()))
}

fn read_regular_file_checked(
    path: &Path,
    validate_metadata: impl FnOnce(&fs::Metadata) -> Result<()>,
) -> Result<String> {
    #[cfg(unix)]
    let file = {
        let descriptor = rustix::fs::open(
            path,
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::CLOEXEC | rustix::fs::OFlags::NOFOLLOW,
            rustix::fs::Mode::empty(),
        )
        .map_err(io::Error::from)
        .with_context(|| format!("opening {}", path.display()))?;
        fs::File::from(descriptor)
    };
    #[cfg(not(unix))]
    let file = fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;

    let metadata = file
        .metadata()
        .with_context(|| format!("inspecting {}", path.display()))?;
    ensure!(
        metadata.is_file(),
        "{} must be a regular file",
        path.display()
    );
    #[cfg(unix)]
    ensure!(
        metadata.nlink() == 1,
        "{} must not have additional hard links",
        path.display()
    );
    validate_metadata(&metadata)?;
    let mut contents = String::new();
    file.take((MAX_USER_CONFIG_BYTES + 1) as u64)
        .read_to_string(&mut contents)
        .with_context(|| format!("reading {}", path.display()))?;
    ensure!(
        contents.len() <= MAX_USER_CONFIG_BYTES,
        "{} exceeds the {} byte configuration limit",
        path.display(),
        MAX_USER_CONFIG_BYTES
    );
    Ok(contents)
}

fn ensure_private_dir(path: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    builder.mode(0o700);
    match builder.create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error).with_context(|| format!("creating {}", path.display())),
    }

    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("checking {}", path.display()))?;
    ensure!(
        metadata.is_dir() && owned_by_current_user(&metadata),
        "{} must be a real directory owned by the current user",
        path.display()
    );
    #[cfg(unix)]
    ensure!(
        metadata.permissions().mode() & 0o077 == 0,
        "{} must deny group and other access",
        path.display()
    );
    Ok(())
}

fn ensure_user_config_parent(path: &Path) -> Result<()> {
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error).with_context(|| format!("creating {}", path.display())),
    }
    validate_user_config_parent(path)
}

fn validate_user_config_parent(path: &Path) -> Result<()> {
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("checking {}", path.display()))?;
    ensure!(
        metadata.is_dir() && owned_by_current_user(&metadata),
        "{} must be a real directory owned by the current user",
        path.display()
    );
    #[cfg(unix)]
    ensure!(
        metadata.permissions().mode() & 0o022 == 0,
        "{} must not be writable by group or other users",
        path.display()
    );
    Ok(())
}

fn validate_private_dir(path: &Path) -> Result<()> {
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("checking {}", path.display()))?;
    ensure!(
        metadata.is_dir() && owned_by_current_user(&metadata),
        "{} must be a real directory owned by the current user",
        path.display()
    );
    #[cfg(unix)]
    ensure!(
        metadata.permissions().mode() & 0o077 == 0,
        "{} must deny group and other access",
        path.display()
    );
    Ok(())
}

#[cfg(unix)]
fn owned_by_current_user(metadata: &fs::Metadata) -> bool {
    metadata.uid() == geteuid().as_raw()
}

#[cfg(not(unix))]
fn owned_by_current_user(_: &fs::Metadata) -> bool {
    true
}
