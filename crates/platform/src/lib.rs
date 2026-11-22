//! Home-relative paths and application configuration for mcbtodo.

pub mod agent_policy;
mod device_identity;
#[cfg(target_os = "linux")]
pub mod local_transport;
#[cfg(unix)]
pub mod private_file;
#[cfg(target_os = "linux")]
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

/// User preferences shared by the application entry points.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct AppConfig {
    /// IANA time zone used for calendar operations.
    pub timezone: Tz,
    /// Initial TUI view, either the calendar month or the last saved view.
    pub home_view: HomeViewPolicy,
    /// Color palette used by the TUI.
    pub tui_theme: TuiTheme,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HomeViewPolicy {
    #[default]
    Month,
    RestoreLastView,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TuiTheme {
    #[default]
    Default,
    HighContrast,
    Monochrome,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            timezone: chrono_tz::UTC,
            home_view: HomeViewPolicy::Month,
            tui_theme: TuiTheme::Default,
        }
    }
}

/// A configuration draft's typed baseline and original file text.
///
/// A missing file has the default baseline. A readable, invalid file retains
/// its parse error and can be replaced explicitly by an editor.
pub struct ConfigSnapshot<T> {
    path: PathBuf,
    source: Option<String>,
    value: Result<T>,
}

impl<T: Default> ConfigSnapshot<T> {
    fn new(
        path: PathBuf,
        source: Option<String>,
        parse: impl FnOnce(&str, &Path) -> Result<T>,
    ) -> Self {
        let value = source
            .as_deref()
            .map_or_else(|| Ok(T::default()), |text| parse(text, &path));
        Self {
            path,
            source,
            value,
        }
    }
}

impl<T> ConfigSnapshot<T> {
    /// Inspect the parsed value, or the error that made the file invalid.
    pub fn value(&self) -> Result<&T, &anyhow::Error> {
        self.value.as_ref()
    }

    /// Consume the snapshot when the caller only needs the parsed configuration.
    pub fn into_value(self) -> Result<T> {
        self.value
    }

    /// Publish a changed or repaired draft. Encode and validate the draft only
    /// when a replacement is needed, then compare the original text on save.
    pub fn save(
        &self,
        paths: &WorkspacePaths,
        draft: &T,
        encode: impl FnOnce(&T) -> Result<String>,
    ) -> Result<bool>
    where
        T: PartialEq,
    {
        let replacement = (self.value().ok() != Some(draft))
            .then(|| encode(draft))
            .transpose()?;
        paths.save_config_file(&self.path, self.source.as_deref(), replacement.as_deref())
    }
}

/// The preferences and file text captured when opening the Settings editor.
pub type AppConfigSnapshot = ConfigSnapshot<AppConfig>;

/// The file no longer matches the snapshot captured by an editor.
#[derive(Debug)]
pub struct ConfigConflict;

impl std::fmt::Display for ConfigConflict {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Config changed on disk")
    }
}

impl std::error::Error for ConfigConflict {}

impl AppConfigSnapshot {
    /// Returns defaults for an absent file and `None` for a readable but invalid file.
    pub fn preferences(&self) -> Option<AppConfig> {
        self.value().ok().copied()
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

    pub fn codex_provider_path(&self) -> PathBuf {
        self.config_dir().join("codex.toml")
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

    /// Whether `root` resolves to the configured default Vault directory.
    pub fn is_default_vault(&self, root: &Path) -> Result<bool> {
        let default_path = self.vault_root();
        let default_root = match default_path.canonicalize() {
            Ok(path) => path,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("resolving the default Vault at {}", default_path.display())
                });
            }
        };
        let root = root
            .canonicalize()
            .with_context(|| format!("resolving Vault at {}", root.display()))?;
        Ok(default_root.is_dir() && root.is_dir() && root == default_root)
    }

    pub fn socket_path(&self) -> PathBuf {
        self.state_dir().join("mcbtodo-daemon.sock")
    }

    pub fn cache_dir(&self) -> PathBuf {
        self.data_dir().join("cache")
    }

    /// Read the main user configuration.
    pub fn load_config(&self) -> Result<AppConfig> {
        let path = self.config_path();
        Self::parse_config(&self.read_config_file(&path)?, &path)
    }

    /// Read the main configuration, distinguishing absence from invalid configuration.
    pub fn load_optional_config(&self) -> Result<Option<AppConfig>> {
        let path = self.config_path();
        self.read_optional_config_file(&path)?
            .as_deref()
            .map(|contents| Self::parse_config(contents, &path))
            .transpose()
    }

    /// Capture an editable snapshot. A read error still prevents editing.
    pub fn read_app_config_snapshot(&self) -> Result<AppConfigSnapshot> {
        self.read_config_snapshot(self.config_path(), Self::parse_config)
    }

    /// Capture one settings file with its parser. A read error prevents editing.
    pub fn read_config_snapshot<T: Default>(
        &self,
        path: PathBuf,
        parse: impl FnOnce(&str, &Path) -> Result<T>,
    ) -> Result<ConfigSnapshot<T>> {
        let source = self.read_optional_config_file(&path)?;
        Ok(ConfigSnapshot::new(path, source, parse))
    }

    /// Read a user configuration file, treating only absence as an empty snapshot.
    pub fn read_optional_config_file(&self, path: &Path) -> Result<Option<String>> {
        match self.read_config_file(path) {
            Ok(contents) => Ok(Some(contents)),
            Err(error) if is_not_found(&error) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Atomically publish changed settings if disk still matches the caller's snapshot.
    ///
    /// Returns `true` if the file was written. An unchanged save preserves the file
    /// or its absence. Modified saves are serialized. External editors can still
    /// write between comparison and publication.
    pub fn save_config(&self, expected: &AppConfigSnapshot, config: AppConfig) -> Result<bool> {
        expected.save(self, &config, |config| {
            toml::to_string(config).context("serializing app config")
        })
    }

    /// Compare the file with its raw snapshot and optionally publish replacement bytes.
    ///
    /// Passing `None` for `replacement` checks freshness without modifying the file
    /// or creating a lock. Callers validate and serialize their own configuration.
    /// Returns `true` if replacement bytes were published.
    fn save_config_file(
        &self,
        path: &Path,
        expected: Option<&str>,
        replacement: Option<&str>,
    ) -> Result<bool> {
        self.validate_config_path(path)?;
        let _lock = if let Some(contents) = replacement {
            ensure!(
                contents.len() <= MAX_USER_CONFIG_BYTES,
                "Config exceeds the {MAX_USER_CONFIG_BYTES} byte limit"
            );
            self.prepare_config_dir()?;
            let lock_path = path.with_extension("lock");
            #[cfg(unix)]
            let lock = private_file::open_private_file(&lock_path)?;
            #[cfg(not(unix))]
            let lock = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&lock_path)
                .with_context(|| format!("opening {}", lock_path.display()))?;
            lock.try_lock().context("Config save busy or unavailable")?;
            Some(lock)
        } else {
            None
        };
        let current = self.read_optional_config_file(path)?;
        if current.as_deref() != expected {
            return Err(ConfigConflict.into());
        }
        let Some(contents) = replacement else {
            return Ok(false);
        };
        let temporary = stage_private_file(path, contents.as_bytes())?;
        if current.is_some() {
            temporary
                .persist(path)
                .with_context(|| format!("publishing {}", path.display()))?;
        } else {
            publish_new_config(&temporary, path)
                .with_context(|| format!("publishing {}", path.display()))?;
        }
        Ok(true)
    }

    /// Read a direct child of the user config directory after validating its directories.
    ///
    /// The `.config` directory may be readable by other users, but not writable
    /// by them. The application config directory must be private; its files must
    /// be current-user-owned and not writable by group or other users.
    pub fn read_config_file(&self, path: &Path) -> Result<String> {
        #[cfg(unix)]
        {
            let bytes = self.read_workspace_config_bytes(path, ConfigFileKind::Preferences)?;
            String::from_utf8(bytes).with_context(|| format!("reading {}", path.display()))
        }
        #[cfg(not(unix))]
        {
            self.validate_config_path(path)?;
            validate_user_config_parent(&self.home.join(".config"))
                .and_then(|()| validate_private_dir(&self.config_dir()))
                .with_context(|| format!("reading {}", path.display()))?;
            read_regular_file(path)
        }
    }

    #[cfg(unix)]
    fn read_workspace_config_bytes(&self, path: &Path, kind: ConfigFileKind) -> Result<Vec<u8>> {
        use rustix::fs::{Mode, OFlags, open, openat};

        self.validate_config_path(path)?;
        let name = path
            .file_name()
            .context("expected a configuration filename")?;
        let config_parent = self.home.join(".config");
        let directory_flags =
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let parent = fs::File::from(
            open(&config_parent, directory_flags, Mode::empty())
                .map_err(io::Error::from)
                .with_context(|| {
                    format!(
                        "opening {} to read {}",
                        config_parent.display(),
                        path.display()
                    )
                })?,
        );
        validate_config_mode(&parent.metadata()?, &config_parent, 0o022)?;
        let config_dir = self.config_dir();
        let directory = fs::File::from(
            openat(
                &parent,
                config_dir
                    .file_name()
                    .expect("workspace config directory has a name"),
                directory_flags,
                Mode::empty(),
            )
            .map_err(io::Error::from)
            .with_context(|| format!("opening {}", config_dir.display()))?,
        );
        validate_config_mode(&directory.metadata()?, &config_dir, kind.directory_mask())?;

        let file = fs::File::from(
            openat(
                &directory,
                name,
                OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
                Mode::empty(),
            )
            .map_err(io::Error::from)
            .with_context(|| format!("opening {}", path.display()))?,
        );
        let metadata = file.metadata()?;
        ensure!(
            metadata.is_file(),
            "{} must be a regular file",
            path.display()
        );
        validate_config_mode(&metadata, path, 0o022)?;
        if kind == ConfigFileKind::Preferences {
            ensure!(
                metadata.nlink() == 1,
                "{} must not have additional hard links",
                path.display()
            );
        }
        let limit = kind.byte_limit();
        let mut bytes = Vec::new();
        file.take((limit + 1) as u64)
            .read_to_end(&mut bytes)
            .with_context(|| format!("reading {}", path.display()))?;
        ensure!(
            bytes.len() <= limit,
            "{} exceeds the configuration size limit of {limit} bytes",
            path.display()
        );
        Ok(bytes)
    }

    fn validate_config_path(&self, path: &Path) -> Result<()> {
        ensure!(
            path.parent() == Some(self.config_dir().as_path()),
            "{} is not a direct child of the mcbtodo config directory",
            path.display()
        );
        Ok(())
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
        stage_private_file(path, contents)?
            .persist(path)
            .with_context(|| format!("publishing {}", path.display()))?;
        Ok(())
    }

    /// Create private config, state and Vault directories and install the default config.
    ///
    /// An existing config is preserved. On Unix, existing application directories must
    /// deny group and other access. Application directories cannot be symlinks.
    pub fn initialize(&self) -> Result<()> {
        self.initialize_config_file().map(|_| ())
    }

    /// Initialize the workspace and parse the configuration without reading it again.
    pub fn initialize_config(&self) -> Result<AppConfig> {
        self.initialize_config_snapshot()?.into_value()
    }

    /// Initialize the workspace and capture the configuration for the TUI.
    pub fn initialize_config_snapshot(&self) -> Result<AppConfigSnapshot> {
        Ok(ConfigSnapshot::new(
            self.config_path(),
            Some(self.initialize_config_file()?),
            Self::parse_config,
        ))
    }

    fn initialize_config_file(&self) -> Result<String> {
        ensure_private_dir(&self.data_dir())?;
        self.prepare_config_dir()?;
        ensure_private_dir(&self.state_dir())?;
        ensure_private_dir(&self.vault_root())?;

        let path = self.config_path();
        match self.read_config_file(&path) {
            Ok(contents) => return Ok(contents),
            Err(error) if is_not_found(&error) => {}
            Err(error) => return Err(error),
        }
        let contents = toml::to_string(&AppConfig::default()).context("serializing app config")?;
        let temporary = stage_private_file(&path, contents.as_bytes())?;
        match publish_new_config(&temporary, &path) {
            Ok(()) => Ok(contents),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                self.read_config_file(&path)
            }
            Err(error) => Err(error).with_context(|| format!("installing {}", path.display())),
        }
    }

    fn prepare_config_dir(&self) -> Result<()> {
        ensure_user_config_parent(&self.home.join(".config"))?;
        ensure_private_dir(&self.config_dir())
    }

    fn parse_config(contents: &str, path: &Path) -> Result<AppConfig> {
        toml::from_str(contents).with_context(|| format!("parsing {}", path.display()))
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
#[derive(Clone, Copy, PartialEq, Eq)]
enum ConfigFileKind {
    Preferences,
    #[cfg(target_os = "linux")]
    Agent,
}

#[cfg(unix)]
impl ConfigFileKind {
    fn directory_mask(self) -> u32 {
        match self {
            Self::Preferences => 0o077,
            #[cfg(target_os = "linux")]
            Self::Agent => 0o022,
        }
    }

    fn byte_limit(self) -> usize {
        match self {
            Self::Preferences => MAX_USER_CONFIG_BYTES,
            #[cfg(target_os = "linux")]
            Self::Agent => agent_policy::MAX_POLICY_BYTES,
        }
    }
}

#[cfg(unix)]
fn validate_config_mode(metadata: &fs::Metadata, path: &Path, forbidden: u32) -> Result<()> {
    ensure!(
        owned_by_current_user(metadata),
        "{} must be owned by the current user",
        path.display()
    );
    ensure!(
        metadata.mode() & forbidden == 0,
        "{} has unsafe group or other permissions",
        path.display()
    );
    Ok(())
}

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

fn is_not_found(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<io::Error>()
        .is_some_and(|error| error.kind() == io::ErrorKind::NotFound)
}

fn stage_private_file(path: &Path, contents: &[u8]) -> Result<NamedTempFile> {
    let mut temporary = NamedTempFile::new_in(path.parent().context("file has no parent")?)
        .with_context(|| format!("staging {}", path.display()))?;
    temporary
        .write_all(contents)
        .and_then(|()| temporary.as_file().sync_all())
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(temporary)
}

fn publish_new_config(temporary: &NamedTempFile, path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use rustix::fs::{Mode, OFlags, RenameFlags, open, renameat_with};

        // A hard-link publication would temporarily violate the config reader's
        // one-link invariant for concurrent readers.
        let directory = open(
            path.parent().expect("config has a parent"),
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::DIRECTORY | OFlags::NOFOLLOW,
            Mode::empty(),
        )?;
        renameat_with(
            &directory,
            temporary.path().file_name().expect("tempfile has a name"),
            &directory,
            path.file_name().expect("config has a name"),
            RenameFlags::NOREPLACE,
        )?;
        Ok(())
    }
    #[cfg(not(unix))]
    fs::hard_link(temporary.path(), path)
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
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::NONBLOCK,
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
    #[cfg(unix)]
    let created = fs::DirBuilder::new().mode(0o700).create(path);
    #[cfg(not(unix))]
    let created = fs::create_dir(path);

    match created {
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
