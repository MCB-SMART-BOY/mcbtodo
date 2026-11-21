use std::collections::{BTreeMap, BTreeSet};

use super::*;

/// Prefix reserved for temporary graph publication directories.
pub const GRAPH_STAGE_PREFIX: &str = ".mcbtodo-graph-";

#[derive(Debug)]
pub struct NewWorkUnit {
    pub unit: WorkUnit,
    pub title: String,
    pub body: String,
}

#[derive(Debug)]
pub struct AddWorkUnitGraph {
    pub project_id: Id,
    pub task_id: Id,
    pub expected_task: Revision,
    pub expected_related: BTreeMap<Id, Revision>,
    pub units: Vec<NewWorkUnit>,
}

impl VaultStore {
    /// Adds a reviewed batch to one Task with a single directory publication.
    /// Existing entries must be regular files in a flat `units` directory.
    pub fn add_work_unit_graph(&self, request: AddWorkUnitGraph) -> Result<(), WriteError> {
        let (lock, vault) = self.locked_snapshot()?;
        let project = validate(&vault, &request)?;
        let parent = project.parent().expect("project document has a parent");
        let destination = parent.join("units");

        publish(parent, &destination, &request.units, || {
            // Recheck the global UUID namespace, graph and reviewed documents
            // in case an external editor bypassed the writer lock while staging.
            let current =
                Vault::load(&lock.root, self.timezone).map_err(WriteError::InvalidVault)?;
            if validate(&current, &request)? != project {
                return Err(WriteError::StaleRevision {
                    id: request.project_id,
                });
            }
            Ok(())
        })
    }
}

fn validate<'a>(vault: &'a Vault, request: &AddWorkUnitGraph) -> Result<&'a Path, WriteError> {
    let project_id = request.project_id;
    let task_id = request.task_id;
    let project = vault
        .projects
        .get(&project_id)
        .ok_or(WriteError::ProjectNotFound { id: project_id })?;
    let task = vault
        .tasks
        .get(&task_id)
        .ok_or(WriteError::TaskNotFound { id: task_id })?;
    check_revision(vault, task_id, &request.expected_task)?;
    let mut added = BTreeSet::new();
    for entry in &request.units {
        let unit = &entry.unit;
        check_new_id(vault, unit.id)?;
        if !added.insert(unit.id) {
            return Err(WriteError::DuplicateId { id: unit.id });
        }
    }
    let required: BTreeSet<_> = std::iter::once(project_id)
        .chain(
            request
                .units
                .iter()
                .flat_map(|entry| entry.unit.depends_on.iter().copied())
                .filter(|id| !added.contains(id)),
        )
        .collect();
    (|| {
        ensure!(!added.is_empty(), "the graph batch is empty");
        ensure!(
            task.data.project_id == project_id
                && request.units.iter().all(|entry| {
                    entry.unit.project_id == project_id && entry.unit.parent_task_id == task_id
                }),
            "graph units must belong to the reviewed task and project"
        );
        ensure!(
            required.iter().eq(request.expected_related.keys()),
            "graph batch requires revisions for the project and each existing prerequisite"
        );
        Ok(())
    })()
    .map_err(WriteError::InvalidDocument)?;
    for (&id, revision) in &request.expected_related {
        check_revision(vault, id, revision)?;
    }
    TaskGraph::build(
        vault
            .units
            .values()
            .map(|doc| &doc.data)
            .chain(request.units.iter().map(|entry| &entry.unit)),
    )
    .map_err(|error| WriteError::InvalidDocument(error.into()))?;
    Ok(&project.path)
}

#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
struct DirectorySnapshot {
    identity: (u64, u64),
    mode: u32,
    entries: BTreeMap<std::ffi::OsString, (u64, u64)>,
}

#[cfg(unix)]
impl DirectorySnapshot {
    fn read(directory: &Path) -> Result<Option<Self>> {
        use std::os::unix::fs::MetadataExt;

        let metadata = match fs::symlink_metadata(directory) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        ensure!(
            metadata.is_dir(),
            "expected directory at {}",
            directory.display()
        );
        let mut entries = BTreeMap::new();
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let file = entry.metadata()?;
            ensure!(
                file.is_file(),
                "expected a regular file at {}",
                entry.path().display()
            );
            entries.insert(entry.file_name(), (file.dev(), file.ino()));
        }
        Ok(Some(Self {
            identity: (metadata.dev(), metadata.ino()),
            mode: metadata.mode(),
            entries,
        }))
    }
}

#[cfg(unix)]
fn publish(
    parent: &Path,
    destination: &Path,
    units: &[NewWorkUnit],
    verify: impl FnOnce() -> Result<(), WriteError>,
) -> Result<(), WriteError> {
    use std::os::unix::fs::PermissionsExt;

    let parent_dir = open_directory(parent).map_err(WriteError::Storage)?;
    let before = DirectorySnapshot::read(destination).map_err(WriteError::Storage)?;
    let staged = tempfile::Builder::new()
        .prefix(GRAPH_STAGE_PREFIX)
        .tempdir_in(parent)
        .map_err(|error| storage(error, "staging graph in", parent))?;
    if let Some(before) = &before {
        // Hard links retain exact file bytes, modes and identity. In-place
        // editor writes to unchanged entries remain visible after the swap.
        for name in before.entries.keys() {
            fs::hard_link(destination.join(name), staged.path().join(name)).map_err(|error| {
                storage(error, "staging existing file", &destination.join(name))
            })?;
        }
    }
    for entry in units {
        let id = entry.unit.id;
        let path = staged.path().join(format!("{id}.md"));
        let source = document::render(
            "mcbtodo.work-unit/v1",
            &entry.unit,
            &entry.title,
            &entry.body,
        )
        .map_err(WriteError::InvalidDocument)?;
        stage(staged.path(), &source, None)
            .map_err(WriteError::Storage)?
            .persist_noclobber(&path)
            .map_err(|error| publish_error(error.error, &destination.join(format!("{id}.md"))))?;
    }
    let stage_dir = open_directory(staged.path()).map_err(WriteError::Storage)?;
    if let Some(before) = &before {
        let linked = DirectorySnapshot::read(staged.path())
            .map_err(WriteError::Storage)?
            .expect("staged directory exists");
        if !before
            .entries
            .iter()
            .all(|(name, identity)| linked.entries.get(name) == Some(identity))
        {
            return Err(WriteError::Storage(anyhow!(
                "units changed while staging the graph"
            )));
        }
        fs::set_permissions(staged.path(), fs::Permissions::from_mode(before.mode))
            .map_err(|error| storage(error, "setting permissions for", staged.path()))?;
    }
    sync_directory(stage_dir).map_err(|error| storage(error, "syncing", staged.path()))?;

    verify()?;
    if DirectorySnapshot::read(destination).map_err(WriteError::Storage)? != before {
        return Err(WriteError::Storage(anyhow!(
            "units changed while staging the graph"
        )));
    }
    if before.is_some() {
        exchange(staged.path(), destination)
            .map_err(|error| storage(error, "publishing graph at", destination))?;
    } else {
        rename_directory_noclobber(staged.path(), destination)
            .map_err(|error| publish_error(error, destination))?;
    }
    finish_publication(staged, destination, before, || sync_directory(parent_dir))
}

#[cfg(unix)]
fn finish_publication(
    staged: tempfile::TempDir,
    destination: &Path,
    before: Option<DirectorySnapshot>,
    sync: impl FnOnce() -> io::Result<()>,
) -> Result<(), WriteError> {
    let result = (|| {
        sync().with_context(|| format!("syncing the parent of {}", destination.display()))?;
        if let Some(expected) = &before {
            ensure!(
                DirectorySnapshot::read(staged.path())?.as_ref() == Some(expected),
                "the displaced units directory changed during publication"
            );
        }
        Ok(())
    })();
    if let Err(source) = result {
        let source = if before.is_some() {
            source.context(format!(
                "previous units retained at {}",
                staged.keep().display()
            ))
        } else {
            source
        };
        return Err(WriteError::GraphPublicationUncertain {
            directory: destination.to_owned(),
            source,
        });
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
fn exchange(from: &Path, to: &Path) -> io::Result<()> {
    use rustix::fs::{CWD, RenameFlags, renameat_with};
    renameat_with(CWD, from, CWD, to, RenameFlags::EXCHANGE).map_err(Into::into)
}

#[cfg(all(
    unix,
    not(any(target_os = "linux", target_os = "android", target_vendor = "apple"))
))]
fn exchange(_from: &Path, _to: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "atomic units directory exchange unavailable",
    ))
}

#[cfg(not(unix))]
fn publish(
    _parent: &Path,
    _destination: &Path,
    _units: &[NewWorkUnit],
    _verify: impl FnOnce() -> Result<(), WriteError>,
) -> Result<(), WriteError> {
    Err(WriteError::Storage(anyhow!(
        "atomic graph publication is unsupported on this platform"
    )))
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn sync_failure_keeps_the_displaced_directory_and_reports_uncertainty() {
        let parent = tempfile::tempdir().unwrap();
        let destination = parent.path().join("units");
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("old.md"), "old").unwrap();
        let before = DirectorySnapshot::read(&destination).unwrap();
        let staged = tempfile::tempdir_in(parent.path()).unwrap();
        let displaced = staged.path().to_owned();
        fs::hard_link(destination.join("old.md"), staged.path().join("old.md")).unwrap();
        fs::write(staged.path().join("new.md"), "new").unwrap();
        exchange(staged.path(), &destination).unwrap();

        let result = finish_publication(staged, &destination, before, || {
            Err(io::Error::other("injected sync failure"))
        });
        assert!(matches!(
            result,
            Err(WriteError::GraphPublicationUncertain { .. })
        ));
        assert_eq!(
            fs::read_to_string(destination.join("new.md")).unwrap(),
            "new"
        );
        assert_eq!(fs::read_to_string(displaced.join("old.md")).unwrap(), "old");
    }
}
