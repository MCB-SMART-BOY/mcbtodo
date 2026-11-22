//! Validated, private directory backups of the authoritative workspace data.

use std::{
    collections::BTreeMap,
    fs::{self, DirBuilder, File, Metadata, OpenOptions, TryLockError},
    io::{self, BufReader, Read, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use mcbtodo_platform::{
    WorkspacePaths,
    agent_policy::{AgentPolicy, MAX_POLICY_BYTES},
};
use mcbtodo_storage_sqlite::workspace_snapshot::{self, SnapshotKind};
use mcbtodo_vault::VaultStore;
use mcbtodo_workflow::RevisionStore;
use rustix::fs::{AtFlags, CWD, Dir, FileType, Mode, OFlags, RenameFlags};
use sha2::{Digest, Sha256};

use crate::proposal;

const MANIFEST: &str = "mcbtodo-backup-v1.json";
const PAYLOAD: &str = "data";
const WORKFLOWS: &str = "workflows";
const REVISIONS: &str = "revisions";
// Match Endpoint::bind in mcbtodo-daemon. Keep the inode on disk after unlocking.
const DAEMON_LOCK: &str = "mcbtodo-daemon.lock";
// Match VaultStore writers.
const VAULT_LOCK: &str = ".mcbtodo-vault.lock";

fn databases(paths: &WorkspacePaths) -> [(SnapshotKind, PathBuf); 2] {
    [
        (SnapshotKind::Focus, paths.focus_database_path()),
        (SnapshotKind::Workflow, paths.workflow_database_path()),
    ]
}

#[derive(Clone, Copy)]
struct DirectoryIdentity {
    device: u64,
    inode: u64,
}

impl DirectoryIdentity {
    fn of(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

pub(crate) fn backup(paths: &WorkspacePaths, destination: &Path) -> Result<()> {
    let destination = Destination::check(destination)?;
    let state_dir = paths.state_dir();
    let config = paths.config_path();
    let default_vault = paths.vault_root();
    let vault = default_vault
        .canonicalize()
        .context("resolving the default Vault")?;
    let vault_identity = DirectoryIdentity::of(
        &fs::metadata(&default_vault).context("checking the default Vault directory")?,
    );
    ensure!(
        default_vault.canonicalize()? == vault,
        "the default Vault path changed while resolving its directory"
    );
    for source in [parent(&config)?, &vault, &state_dir] {
        let canonical = source
            .canonicalize()
            .with_context(|| format!("opening {}", source.display()))?;
        ensure!(
            !destination.path.starts_with(&canonical),
            "backup destination must be outside {}",
            canonical.display(),
        );
    }
    let _daemon_lock = lock_daemon_state(&state_dir)?;
    let staging = tempfile::tempdir_in(parent(&destination.path)?)?;
    let bundle = staging.path().join("bundle");
    let data = bundle.join(PAYLOAD);
    let captured = WorkspacePaths::from_home(data.clone());
    capture(paths, &captured, &vault, vault_identity)?;
    validate(&captured)?;

    let mut manifest = private_file(&bundle.join(MANIFEST), 0o600)?;
    serde_json::to_writer_pretty(&mut manifest, &inventory(&data)?)?;
    manifest.write_all(b"\n")?;
    manifest.sync_all()?;
    sync_directory(&bundle)?;
    destination.publish(&bundle)
}

pub(crate) fn restore(source: &Path, destination_home: &Path) -> Result<()> {
    let destination = Destination::check(destination_home)?;
    let source_dir = open_directory(source)?;
    let canonical_source = source
        .canonicalize()
        .context("resolving backup directory")?;
    ensure!(
        !destination.path.starts_with(&canonical_source),
        "restore destination must be outside the backup directory"
    );
    let manifest =
        open_regular_at(&source_dir, Path::new(MANIFEST)).context("opening backup manifest")?;
    let expected: BTreeMap<String, String> =
        serde_json::from_reader(BufReader::new(manifest)).context("reading backup manifest")?;
    let data = open_directory_at(&source_dir, Path::new(PAYLOAD)).context("opening backup data")?;
    let staging = tempfile::tempdir_in(parent(&destination.path)?)?;
    let restored_home = staging.path().join("home");
    let restored = WorkspacePaths::from_home(restored_home.clone());
    copy_tree(
        &data,
        &restored_home,
        Tree::Backup {
            policy: &restored.agent_policy_path(),
        },
    )
    .context("staging backup data")?;
    ensure!(
        inventory(&restored_home)? == expected,
        "backup data does not match the manifest"
    );
    validate(&restored)?;
    destination.publish(&restored_home)
}

fn capture(
    source: &WorkspacePaths,
    destination: &WorkspacePaths,
    source_vault: &Path,
    expected_vault: DirectoryIdentity,
) -> Result<()> {
    let proposals = proposal::lock_for_snapshot(source)?;
    let config = source.config_path();
    let config_dir = open_directory(parent(&config)?)?;
    let captured_config = destination.config_path();
    private_dirs(parent(&captured_config)?)?;
    let mut file = open_regular_at(
        &config_dir,
        Path::new(config.file_name().context("config filename")?),
    )?;
    copy_file(&mut file, &captured_config)?;
    capture_agent_policy(source, destination, &config_dir)?;
    let vault = open_directory(source_vault)?;
    let opened_vault = DirectoryIdentity::of(&vault.metadata()?);
    ensure!(
        opened_vault.device == expected_vault.device && opened_vault.inode == expected_vault.inode,
        "the default Vault directory changed while preparing the backup"
    );
    let _vault_lock = lock_vault(&vault)?;
    copy_tree(&vault, &destination.vault_root(), Tree::VaultRoot)?;

    let state = source.state_dir();
    let captured_state = destination.state_dir();
    private_dirs(&captured_state)?;
    proposals.copy_to(destination)?;
    for ((_, source), (_, destination)) in databases(source).into_iter().zip(databases(destination))
    {
        snapshot_database(&source, &destination)
            .with_context(|| format!("capturing SQLite database {}", source.display()))?;
    }

    let workflows = state.join(WORKFLOWS);
    if metadata_if_present(&workflows)?.is_some() {
        let workflows = open_directory(&workflows)?;
        let directory = match open_directory_at(&workflows, Path::new(REVISIONS)) {
            Ok(directory) => directory,
            Err(rustix::io::Errno::NOENT) => return Ok(()),
            Err(error) => return Err(error).context("opening published revisions directory"),
        };
        let captured_revisions = captured_state.join(WORKFLOWS).join(REVISIONS);
        private_dirs(&captured_revisions)?;
        for entry in Dir::read_from(&directory)? {
            let entry = entry?;
            let name = Path::new(std::ffi::OsStr::from_bytes(entry.file_name().to_bytes()));
            if revision_hash(name).is_some() {
                let mut file = open_regular_at(&directory, name)
                    .with_context(|| format!("opening published revision {}", name.display()))?;
                copy_file(&mut file, &captured_revisions.join(name))?;
            }
        }
    }
    Ok(())
}

fn capture_agent_policy(
    source: &WorkspacePaths,
    destination: &WorkspacePaths,
    config_dir: &File,
) -> Result<()> {
    let path = source.agent_policy_path();
    let name = Path::new(path.file_name().context("agent policy filename")?);
    match rustix::fs::statat(config_dir, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(_) => (),
        Err(rustix::io::Errno::NOENT) => return Ok(()),
        Err(error) => return Err(error).context("checking agent policy"),
    }
    let config_parent = open_directory(parent(parent(&path)?)?)?;
    let policy = open_regular_at(config_dir, name).context("opening agent policy")?;
    // Backup must not turn a policy the daemon distrusts into an active grant.
    for metadata in [
        config_parent.metadata()?,
        config_dir.metadata()?,
        policy.metadata()?,
    ] {
        ensure!(
            metadata.uid() == rustix::process::geteuid().as_raw() && metadata.mode() & 0o022 == 0,
            "agent policy and its configuration directories must be user-owned and protected from other writers"
        );
    }
    copy_agent_policy(policy, &destination.agent_policy_path())
}

fn copy_agent_policy(policy: File, destination: &Path) -> Result<()> {
    let mut bytes = Vec::new();
    policy
        .take((MAX_POLICY_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .context("reading agent policy")?;
    AgentPolicy::parse(&bytes).context("validating agent policy")?;
    let mut output = private_file(destination, 0o600)?;
    output.write_all(&bytes).context("writing agent policy")?;
    output.sync_all()?;
    Ok(())
}

fn lock_daemon_state(state: &Path) -> Result<File> {
    open_directory(parent(state)?)?;
    let directory = open_directory(state)?;
    let meta = directory.metadata()?;
    ensure!(
        owned_private(&meta),
        "state directory {} must be private and owned by the current user",
        state.display()
    );
    let lock = open_lock(&directory, DAEMON_LOCK).context("opening the daemon state lock")?;
    check_private_file(&lock.metadata()?, &state.join(DAEMON_LOCK))?;
    match lock.try_lock() {
        Ok(()) => Ok(lock),
        Err(TryLockError::WouldBlock) => {
            bail!("mcbtodo-daemon holds the daemon state lock; stop it before backing up")
        }
        Err(TryLockError::Error(error)) => Err(error).context("taking the daemon state lock"),
    }
}

fn lock_vault(directory: &File) -> Result<File> {
    let lock = open_lock(directory, VAULT_LOCK).context("opening the Vault write lock")?;
    let metadata = lock.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.uid() == rustix::process::geteuid().as_raw()
            && metadata.mode() & 0o022 == 0
            && metadata.nlink() == 1,
        "invalid Vault write lock"
    );
    match lock.try_lock() {
        Ok(()) => Ok(lock),
        Err(TryLockError::WouldBlock) => {
            bail!("Vault write lock is held; retry backup after the write")
        }
        Err(TryLockError::Error(error)) => Err(error).context("taking the Vault write lock"),
    }
}

fn open_lock(directory: &File, name: &str) -> rustix::io::Result<File> {
    rustix::fs::openat(
        directory,
        name,
        OFlags::RDWR | OFlags::CREATE | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::RUSR | Mode::WUSR,
    )
    .map(File::from)
}

fn snapshot_database(source: &Path, destination: &Path) -> Result<()> {
    let metadata = metadata_if_present(source)?;
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut sidecar = source.as_os_str().to_os_string();
        sidecar.push(suffix);
        let sidecar = PathBuf::from(sidecar);
        if let Some(meta) = metadata_if_present(&sidecar)? {
            check_private_file(&meta, &sidecar)?;
            ensure!(
                metadata.is_some(),
                "orphaned SQLite sidecar {}",
                sidecar.display()
            );
        }
    }
    let Some(meta) = metadata else { return Ok(()) };
    check_private_file(&meta, source)?;
    private_file(destination, 0o600)?;
    workspace_snapshot::capture(source, destination)
}

fn validate(paths: &WorkspacePaths) -> Result<()> {
    let config = paths.config_path();
    let vault = paths.vault_root();
    let state = paths.state_dir();
    let config_dir = parent(&config)?;
    let config_parent = parent(config_dir)?;
    let workspace = parent(&state)?;
    let root = parent(workspace)?;
    let workflows = state.join(WORKFLOWS);
    let revisions = workflows.join(REVISIONS);
    for (dir, allowed) in [
        (root, vec![config_parent.to_owned(), workspace.to_owned()]),
        (config_parent, vec![config_dir.to_owned()]),
        (config_dir, vec![config.clone(), paths.agent_policy_path()]),
        (workspace, vec![vault.clone(), state.clone()]),
        (
            &state,
            databases(paths)
                .into_iter()
                .map(|(_, path)| path)
                .chain([workflows.clone(), paths.proposals_dir()])
                .collect(),
        ),
    ] {
        check_children(dir, &allowed)?;
    }
    let config = paths.load_config().context("validating backup config")?;
    let policy = paths.agent_policy_path();
    if let Some(metadata) = metadata_if_present(&policy)? {
        check_private_file(&metadata, &policy)?;
    }
    ensure!(
        metadata_if_present(&vault.join(VAULT_LOCK))?.is_none(),
        "unexpected Vault write lock in backup"
    );
    VaultStore::new(&vault, config.timezone)
        .validate_staged_snapshot()
        .context("validating backup Vault")?;
    proposal::validate_snapshot(paths).context("validating backup proposals")?;
    if workflows.exists() {
        check_children(&workflows, std::slice::from_ref(&revisions))?;
        let store = RevisionStore::new(&state);
        for entry in fs::read_dir(&revisions)? {
            let path = entry?.path();
            let hash = revision_hash(&path)
                .with_context(|| format!("invalid revision filename {}", path.display()))?;
            ensure!(path.is_file(), "revision {} must be a file", path.display());
            store
                .load(hash)
                .with_context(|| format!("validating published revision {hash}"))?;
        }
    }
    for (kind, database) in databases(paths) {
        if database.exists() {
            kind.validate(&database)
                .with_context(|| format!("validating SQLite snapshot {}", database.display()))?;
        }
    }
    Ok(())
}

fn revision_hash(path: &Path) -> Option<&str> {
    let hash = path.file_name()?.to_str()?.strip_suffix(".json")?;
    (hash.len() == 64
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    .then_some(hash)
}

fn check_children(directory: &Path, allowed: &[PathBuf]) -> Result<()> {
    for entry in
        fs::read_dir(directory).with_context(|| format!("reading {}", directory.display()))?
    {
        let path = entry?.path();
        ensure!(
            allowed.contains(&path),
            "unexpected backup entry {}",
            path.display()
        );
    }
    Ok(())
}

fn open_directory(path: &Path) -> Result<File> {
    open_directory_at(CWD, path).with_context(|| format!("opening directory {}", path.display()))
}

fn open_directory_at(base: impl std::os::fd::AsFd, path: &Path) -> rustix::io::Result<File> {
    Ok(File::from(rustix::fs::openat(
        base,
        path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::DIRECTORY,
        Mode::empty(),
    )?))
}

fn open_regular_at(directory: &File, path: &Path) -> Result<File> {
    let stat = rustix::fs::statat(directory, path, AtFlags::SYMLINK_NOFOLLOW)?;
    ensure!(
        FileType::from_raw_mode(stat.st_mode).is_file(),
        "expected regular file at {}",
        path.display()
    );
    let file = File::from(rustix::fs::openat(
        directory,
        path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(),
    )?);
    ensure!(
        file.metadata()?.is_file(),
        "expected regular file at {}",
        path.display()
    );
    Ok(file)
}

#[derive(Clone, Copy)]
enum Tree<'a> {
    Backup { policy: &'a Path },
    VaultRoot,
    Projects,
    Project,
    Vault,
}

// Resolve every untrusted entry relative to its open parent and refuse symlinks.
fn copy_tree(source: &File, destination: &Path, tree: Tree<'_>) -> Result<()> {
    private_dirs(destination)?;
    for entry in Dir::read_from(source)? {
        let entry = entry?;
        let bytes = entry.file_name().to_bytes();
        if matches!(bytes, b"." | b"..") {
            continue;
        }
        let name = Path::new(std::ffi::OsStr::from_bytes(bytes));
        if matches!(tree, Tree::VaultRoot) && name == Path::new(VAULT_LOCK) {
            continue;
        }
        let destination = destination.join(name);
        if matches!(tree, Tree::Backup { policy } if destination == policy) {
            let file = open_regular_at(source, name).context("opening backup agent policy")?;
            copy_agent_policy(file, &destination)?;
            continue;
        }
        let metadata = rustix::fs::statat(source, name, AtFlags::SYMLINK_NOFOLLOW)?;
        match FileType::from_raw_mode(metadata.st_mode) {
            FileType::Directory
                if matches!(tree, Tree::Project)
                    && bytes.starts_with(mcbtodo_vault::GRAPH_STAGE_PREFIX.as_bytes()) => {}
            FileType::Directory => copy_tree(
                &open_directory_at(source, name)?,
                &destination,
                match tree {
                    Tree::VaultRoot if bytes == b"projects" => Tree::Projects,
                    Tree::Projects => Tree::Project,
                    Tree::Backup { .. } => tree,
                    _ => Tree::Vault,
                },
            )?,
            FileType::Socket if !matches!(tree, Tree::Backup { .. }) => (),
            _ => {
                let mut file = open_regular_at(source, name)
                    .with_context(|| format!("copying {}", destination.display()))?;
                copy_file(&mut file, &destination)?;
            }
        }
    }
    Ok(())
}

fn copy_file(file: &mut File, destination: &Path) -> Result<()> {
    let mode = 0o600 | (file.metadata()?.mode() & 0o100);
    let mut output = private_file(destination, mode)?;
    io::copy(file, &mut output).with_context(|| format!("writing {}", destination.display()))?;
    output.sync_all()?;
    Ok(())
}

fn private_file(path: &Path, mode: u32) -> Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)
        .with_context(|| format!("creating {}", path.display()))
}

fn private_dirs(path: &Path) -> Result<()> {
    DirBuilder::new()
        .mode(0o700)
        .recursive(true)
        .create(path)
        .with_context(|| format!("creating directory {}", path.display()))
}

fn owned_private(meta: &Metadata) -> bool {
    meta.uid() == rustix::process::geteuid().as_raw() && meta.mode() & 0o077 == 0
}

fn check_private_file(meta: &Metadata, path: &Path) -> Result<()> {
    ensure!(
        meta.is_file() && owned_private(meta) && meta.nlink() == 1,
        "{} must be a private regular file owned by the current user",
        path.display()
    );
    Ok(())
}

// Inventory paths include empty directories. ASCII escaping preserves non-UTF-8
// path bytes and prevents ambiguous names in the portable JSON representation.
fn inventory(root: &Path) -> Result<BTreeMap<String, String>> {
    fn walk(root: &Path, dir: &Path, entries: &mut BTreeMap<String, String>) -> Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let relative = path.strip_prefix(root)?;
            let name = relative.as_os_str().as_bytes().escape_ascii().to_string();
            let kind = entry.file_type()?;
            let value = if kind.is_dir() {
                walk(root, &path, entries)?;
                "directory".to_owned()
            } else {
                ensure!(kind.is_file(), "unexpected file type at {}", path.display());
                let mut digest = Sha256::new();
                io::copy(&mut File::open(&path)?, &mut digest)?;
                hex::encode(digest.finalize())
            };
            entries.insert(name, value);
        }
        sync_directory(dir)
    }

    let mut entries = BTreeMap::new();
    walk(root, root, &mut entries)?;
    Ok(entries)
}

fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)?
        .sync_all()
        .with_context(|| format!("syncing directory {}", path.display()))
}

fn parent(path: &Path) -> Result<&Path> {
    path.parent()
        .with_context(|| format!("{} must have a parent directory", path.display()))
}

fn metadata_if_present(path: &Path) -> Result<Option<Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(meta) => Ok(Some(meta)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("checking {}", path.display())),
    }
}

struct Destination {
    path: PathBuf,
    existing: Option<(u64, u64)>,
}

impl Destination {
    fn check(path: &Path) -> Result<Self> {
        let name = path
            .file_name()
            .context("destination must name a directory")?;
        let parent = parent(path)?;
        let parent = if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        };
        let path = parent
            .canonicalize()
            .context("opening destination parent")?
            .join(name);
        let existing = if let Some(meta) = metadata_if_present(&path)? {
            ensure!(
                meta.is_dir(),
                "destination {} must be a real directory",
                path.display()
            );
            ensure!(
                fs::read_dir(&path)?.next().is_none(),
                "destination {} must be empty",
                path.display()
            );
            Some((meta.dev(), meta.ino()))
        } else {
            None
        };
        Ok(Self { path, existing })
    }

    fn publish(self, staged: &Path) -> Result<()> {
        let install = || format!("publishing {}", self.path.display());
        if let Some(identity) = self.existing {
            ensure!(
                metadata_if_present(&self.path)?
                    .is_some_and(|meta| meta.is_dir() && (meta.dev(), meta.ino()) == identity),
                "destination {} changed during staging",
                self.path.display()
            );
            // rename refuses to replace a nonempty directory, even if populated just now.
            fs::rename(staged, &self.path).with_context(install)?;
        } else {
            rustix::fs::renameat_with(CWD, staged, CWD, &self.path, RenameFlags::NOREPLACE)
                .with_context(install)?;
        }
        sync_directory(parent(&self.path)?)
            .context("destination was installed but its parent could not be synced")
    }
}

#[cfg(test)]
#[path = "backup_tests.rs"]
mod tests;
