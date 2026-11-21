//! Content-addressed publication of validated workflows and their inputs.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{ExecutionPlan, ScriptSources, WorkflowError, compile, parse_definition};

const MAX_SOURCE_BYTES: u64 = 1024 * 1024;
const MAX_INPUT_BYTES: u64 = 2 * 1024 * 1024;
const MAX_BUNDLE_BYTES: u64 = 12 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum PublicationError {
    #[error("workflow file {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("workflow graph {}: {source}", path.display())]
    Workflow {
        path: PathBuf,
        #[source]
        source: WorkflowError,
    },
    #[error("workflow source {name} must be inside {}", root.display())]
    SourceOutsideGraph { name: String, root: PathBuf },
    #[error("invalid workflow revision hash `{0}`: expected 64 lowercase hexadecimal digits")]
    InvalidHash(String),
    #[error("workflow revision hash mismatch: expected {expected}, found {actual}")]
    HashMismatch { expected: String, actual: String },
    #[error("encoding workflow revision: {0}")]
    Encode(#[source] serde_json::Error),
    #[error("workflow revision {}: {source}", path.display())]
    Decode {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

/// A validated snapshot of the exact graph, scripts and executable inputs.
#[derive(Debug)]
pub struct WorkflowRevision {
    hash: String,
    snapshot: Snapshot,
    plan: ExecutionPlan,
}

/// A validated, serializable snapshot captured independently of filesystem paths.
///
/// Deserialization does not imply validity; publication always revalidates it.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowDraft {
    snapshot: Snapshot,
}

impl WorkflowDraft {
    /// Validate graph and exact source bytes without accessing the filesystem.
    pub fn new(graph: String, scripts: ScriptSources) -> Result<Self, PublicationError> {
        let snapshot = Snapshot {
            schema: SnapshotSchema::V1,
            graph,
            scripts,
        };
        validate_snapshot(snapshot.clone())?;
        Ok(Self { snapshot })
    }

    pub fn graph(&self) -> &str {
        &self.snapshot.graph
    }

    pub fn script_sources(&self) -> &ScriptSources {
        &self.snapshot.scripts
    }

    /// SHA-256 of the exact serialized graph and source snapshot.
    pub fn hash(&self) -> Result<String, PublicationError> {
        let bytes = serde_json::to_vec(&self.snapshot).map_err(PublicationError::Encode)?;
        Ok(digest(&bytes))
    }

    /// Revalidate a deserialized snapshot before it enters a trusted workflow.
    pub fn validate(&self) -> Result<(), PublicationError> {
        validate_snapshot(self.snapshot.clone()).map(|_| ())
    }
}

impl WorkflowRevision {
    /// SHA-256 of the stored snapshot. This includes graph formatting and comments.
    ///
    /// Use this hash for storage and run identity. [`ExecutionPlan::revision_hash`]
    /// identifies the normalized plan.
    pub fn hash(&self) -> &str {
        &self.hash
    }

    pub fn plan(&self) -> &ExecutionPlan {
        &self.plan
    }

    pub fn graph_bytes(&self) -> &[u8] {
        self.snapshot.graph.as_bytes()
    }

    /// Exact script and executable bytes keyed by the source paths in the graph.
    pub fn script_sources(&self) -> &ScriptSources {
        &self.snapshot.scripts
    }
}

/// Store beneath an application's state directory.
#[derive(Debug, Clone)]
pub struct RevisionStore {
    state_dir: PathBuf,
}

impl RevisionStore {
    pub fn new(state_dir: impl AsRef<Path>) -> Self {
        Self {
            state_dir: state_dir.as_ref().to_owned(),
        }
    }

    /// Publish a validated draft. Concurrent publishers of the same bytes share
    /// one revision. A corrupt entry at that hash is reported and preserved.
    pub fn publish(
        &self,
        graph_path: impl AsRef<Path>,
    ) -> Result<WorkflowRevision, PublicationError> {
        let (revision, bytes) = capture_draft(graph_path.as_ref())?;
        self.publish_bytes(revision, bytes)
    }

    /// Publish the exact validated snapshot supplied by the caller.
    ///
    /// The snapshot is revalidated here so a deserialized proposal cannot bypass
    /// compiler and source-boundary checks.
    pub fn publish_draft(
        &self,
        draft: &WorkflowDraft,
    ) -> Result<WorkflowRevision, PublicationError> {
        let (revision, bytes) = validate_snapshot(draft.snapshot.clone())?;
        self.publish_bytes(revision, bytes)
    }

    fn publish_bytes(
        &self,
        revision: WorkflowRevision,
        bytes: Vec<u8>,
    ) -> Result<WorkflowRevision, PublicationError> {
        let directory = self.prepare_directory(true)?;
        let destination = directory.join(format!("{}.json", revision.hash()));
        let (staging, mut file) = Stage::new(&directory)?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|error| io_error(&staging.0, error))?;
        drop(file);

        // Linking exposes the complete file in one step and cannot replace an entry.
        match fs::hard_link(&staging.0, &destination) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                self.load(revision.hash())?;
            }
            Err(error) => return Err(io_error(&destination, error)),
        }
        // Remove the staging name before syncing the directory.
        drop(staging);
        sync_directory(&directory)?;
        Ok(revision)
    }

    /// Load by publication hash, verify the stored bytes, then compile the snapshot.
    pub fn load(&self, hash: &str) -> Result<WorkflowRevision, PublicationError> {
        if !valid_hash(hash) {
            return Err(PublicationError::InvalidHash(hash.to_owned()));
        }
        let path = self.prepare_directory(false)?.join(format!("{hash}.json"));
        let bytes = read_file(&path, MAX_BUNDLE_BYTES)?;
        let actual = digest(&bytes);
        if actual != hash {
            return Err(PublicationError::HashMismatch {
                expected: hash.to_owned(),
                actual,
            });
        }
        let snapshot: Snapshot =
            serde_json::from_slice(&bytes).map_err(|source| PublicationError::Decode {
                path: path.clone(),
                source,
            })?;
        let definition =
            parse_definition(&snapshot.graph).map_err(|err| workflow_error(&path, err))?;
        if !definition
            .source_paths()
            .eq(snapshot.scripts.keys().map(String::as_str))
        {
            return Err(invalid_data(
                &path,
                "workflow sources do not match the graph",
            ));
        }
        let plan =
            compile(&definition, &snapshot.scripts).map_err(|err| workflow_error(&path, err))?;
        Ok(WorkflowRevision {
            hash: actual,
            snapshot,
            plan,
        })
    }

    /// List published hashes in lexical order. [`Self::load`] verifies individual revisions.
    pub fn list(&self) -> Result<Vec<String>, PublicationError> {
        let directory = match self.prepare_directory(false) {
            Ok(directory) => directory,
            Err(PublicationError::Io { source, .. })
                if source.kind() == io::ErrorKind::NotFound =>
            {
                return Ok(Vec::new());
            }
            Err(error) => return Err(error),
        };
        let mut hashes = Vec::new();
        for entry in fs::read_dir(&directory).map_err(|err| io_error(&directory, err))? {
            let entry = entry.map_err(|err| io_error(&directory, err))?;
            let name = entry.file_name();
            if let Some(hash) = name.to_str().and_then(|name| name.strip_suffix(".json"))
                && valid_hash(hash)
                && entry
                    .file_type()
                    .map_err(|err| io_error(&entry.path(), err))?
                    .is_file()
            {
                hashes.push(hash.to_owned());
            }
        }
        hashes.sort_unstable();
        Ok(hashes)
    }

    fn prepare_directory(&self, create: bool) -> Result<PathBuf, PublicationError> {
        let workflows = self.state_dir.join("workflows");
        let revisions = workflows.join("revisions");
        for (path, recursive) in [
            (&self.state_dir, true),
            (&workflows, false),
            (&revisions, false),
        ] {
            if create {
                let mut builder = fs::DirBuilder::new();
                builder.recursive(recursive);
                #[cfg(unix)]
                builder.mode(0o700);
                match builder.create(path) {
                    Ok(()) => {}
                    Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(err) => return Err(io_error(path, err)),
                }
            }
            if !fs::symlink_metadata(path)
                .map_err(|err| io_error(path, err))?
                .is_dir()
            {
                return Err(invalid_data(path, "expected a real directory"));
            }
        }
        Ok(revisions)
    }
}

/// Read and compile a draft with the same source validation used by publication.
/// Does not write to the revision store.
pub fn load_draft(graph_path: impl AsRef<Path>) -> Result<WorkflowRevision, PublicationError> {
    capture_draft(graph_path.as_ref()).map(|(revision, _)| revision)
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    schema: SnapshotSchema,
    graph: String,
    scripts: ScriptSources,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
enum SnapshotSchema {
    #[serde(rename = "mcbtodo.workflow-revision/v1")]
    V1,
}

fn capture_draft(graph_path: &Path) -> Result<(WorkflowRevision, Vec<u8>), PublicationError> {
    let path = graph_path
        .canonicalize()
        .map_err(|err| io_error(graph_path, err))?;
    let root = path
        .parent()
        .ok_or_else(|| invalid_data(&path, "expected a containing directory"))?;
    let graph = String::from_utf8(read_file(&path, MAX_SOURCE_BYTES)?)
        .map_err(|err| invalid_data(&path, err))?;
    let definition = parse_definition(&graph).map_err(|err| workflow_error(&path, err))?;
    let mut scripts = ScriptSources::new();
    let script_paths: std::collections::BTreeSet<_> = definition.script_paths().collect();
    let mut remaining = MAX_INPUT_BYTES;
    let mut remaining_script_bytes = MAX_SOURCE_BYTES;
    for name in definition.source_paths() {
        let candidate = root.join(name);
        let source = candidate
            .canonicalize()
            .map_err(|err| io_error(&candidate, err))?;
        if !source.starts_with(root) {
            return Err(PublicationError::SourceOutsideGraph {
                name: name.to_owned(),
                root: root.to_owned(),
            });
        }
        let is_script = script_paths.contains(name);
        let limit = if is_script {
            remaining.min(remaining_script_bytes)
        } else {
            remaining
        };
        let bytes = read_file(&source, limit)?;
        remaining -= bytes.len() as u64;
        if is_script {
            remaining_script_bytes -= bytes.len() as u64;
        }
        scripts.insert(name.to_owned(), bytes);
    }
    let snapshot = Snapshot {
        schema: SnapshotSchema::V1,
        graph,
        scripts,
    };
    validate_snapshot(snapshot).map_err(|error| match error {
        PublicationError::Workflow { source, .. } => workflow_error(&path, source),
        PublicationError::Io { source, .. } => io_error(&path, source),
        error => error,
    })
}

fn validate_snapshot(snapshot: Snapshot) -> Result<(WorkflowRevision, Vec<u8>), PublicationError> {
    let path = Path::new("<workflow snapshot>");
    let graph_bytes = snapshot.graph.len() as u64;
    if graph_bytes > MAX_SOURCE_BYTES {
        return Err(invalid_data(path, "workflow graph size limit exceeded"));
    }
    let definition = parse_definition(&snapshot.graph).map_err(|err| workflow_error(path, err))?;
    let source_paths: std::collections::BTreeSet<_> =
        definition.source_paths().map(str::to_owned).collect();
    if source_paths.len() != snapshot.scripts.len()
        || !source_paths.iter().eq(snapshot.scripts.keys())
    {
        return Err(invalid_data(
            path,
            "workflow sources do not match the graph",
        ));
    }
    let script_paths: std::collections::BTreeSet<_> =
        definition.script_paths().map(str::to_owned).collect();
    let mut total_inputs = 0_u64;
    let mut total_scripts = 0_u64;
    for (name, bytes) in &snapshot.scripts {
        let size = bytes.len() as u64;
        total_inputs += size;
        if script_paths.contains(name) {
            total_scripts += size;
        }
    }
    if total_inputs > MAX_INPUT_BYTES || total_scripts > MAX_SOURCE_BYTES {
        return Err(invalid_data(path, "workflow input size limit exceeded"));
    }
    let plan = compile(&definition, &snapshot.scripts).map_err(|err| workflow_error(path, err))?;
    let bytes = serde_json::to_vec(&snapshot).map_err(PublicationError::Encode)?;
    if bytes.len() as u64 > MAX_BUNDLE_BYTES {
        return Err(invalid_data(path, "workflow snapshot size limit exceeded"));
    }
    let hash = digest(&bytes);
    Ok((
        WorkflowRevision {
            hash,
            snapshot,
            plan,
        },
        bytes,
    ))
}

fn read_file(path: &Path, max_bytes: u64) -> Result<Vec<u8>, PublicationError> {
    if !fs::symlink_metadata(path)
        .map_err(|err| io_error(path, err))?
        .is_file()
    {
        return Err(invalid_data(path, "expected a regular file"));
    }
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| file.take(max_bytes + 1).read_to_end(&mut bytes))
        .map_err(|err| io_error(path, err))?;
    if bytes.len() as u64 > max_bytes {
        return Err(invalid_data(path, "workflow input size limit exceeded"));
    }
    Ok(bytes)
}

fn valid_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn invalid_data(
    path: &Path,
    message: impl Into<Box<dyn std::error::Error + Send + Sync>>,
) -> PublicationError {
    io_error(path, io::Error::new(io::ErrorKind::InvalidData, message))
}

fn io_error(path: &Path, source: io::Error) -> PublicationError {
    PublicationError::Io {
        path: path.to_owned(),
        source,
    }
}

fn workflow_error(path: &Path, source: WorkflowError) -> PublicationError {
    PublicationError::Workflow {
        path: path.to_owned(),
        source,
    }
}

fn sync_directory(path: &Path) -> Result<(), PublicationError> {
    #[cfg(unix)]
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|err| io_error(path, err))?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

struct Stage(PathBuf);

impl Stage {
    fn new(directory: &Path) -> Result<(Self, File), PublicationError> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);

        loop {
            let path = directory.join(format!(
                ".publish-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match options.open(&path) {
                Ok(file) => return Ok((Self(path), file)),
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
                Err(err) => return Err(io_error(&path, err)),
            }
        }
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
