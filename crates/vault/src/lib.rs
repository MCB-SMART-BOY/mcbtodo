mod completion_journal;
mod document;
mod persistence;
mod watch;

pub use persistence::{
    AddWorkUnitGraph, BindWorkflow, CaptureInboxEntry, CaptureTask, CompleteWorkUnit, CreateEvent,
    CreateProject, CreateReminder, CreateTimeBlock, CreateWorkUnit, GRAPH_STAGE_PREFIX,
    MaterializeTaskOccurrences, NewWorkUnit, PersistedDocument, Revision, SetDeadline,
    SetWorkUnitDependencies, TriageInboxEntry, VaultStore, WriteError,
};
pub use watch::VaultWatcher;

use std::{
    collections::BTreeMap,
    fmt::Write as _,
    fs, io,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use chrono::Days;
use chrono_tz::Tz;
use document::Metadata;
use mcbtodo_domain::{
    CalendarEntry, CalendarEvent, GraphError, Id, InboxEntry, Project, Task, TaskGraph, TaskStatus,
    WorkUnit, WorkUnitStatus,
    recurrence::Reminder,
    time::{DateSpan, Span},
};

#[derive(Debug)]
pub struct Document<T> {
    pub path: PathBuf,
    pub title: String,
    pub data: T,
}

#[derive(Debug)]
pub struct Vault {
    timezone: Tz,
    projects: BTreeMap<Id, Document<Project>>,
    tasks: BTreeMap<Id, Document<Task>>,
    units: BTreeMap<Id, Document<WorkUnit>>,
    events: BTreeMap<Id, Document<CalendarEvent>>,
    reminders: BTreeMap<Id, Document<Reminder>>,
    inbox: BTreeMap<Id, Document<InboxEntry>>,
    graph: TaskGraph,
    revisions: BTreeMap<Id, Revision>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Project,
    Task,
    Unit,
    Event,
    Reminder,
    Inbox,
}

#[derive(Debug)]
pub struct OutlineRow<'a> {
    pub id: Id,
    pub depth: usize,
    pub title: &'a str,
    pub path: &'a Path,
    pub blocked: bool,
}

impl Vault {
    pub fn load(root: &Path, timezone: Tz) -> Result<Self> {
        let root = root
            .canonicalize()
            .with_context(|| format!("opening vault {}", root.display()))?;
        ensure!(
            root.is_dir(),
            "expected a vault directory at {}",
            root.display()
        );
        let mut documents = Documents::default();
        for entry in entries(&root.join("projects"))? {
            if !is_kind(&entry, fs::FileType::is_dir)? {
                continue;
            }
            documents.read(&entry.join("project.md"), Kind::Project)?;
            for (subdir, kind) in [("tasks", Kind::Task), ("units", Kind::Unit)] {
                documents.read_markdown(&entry.join(subdir), kind)?;
            }
        }
        documents.read_markdown(&root.join("calendar"), Kind::Event)?;
        documents.read_markdown(&root.join("reminders"), Kind::Reminder)?;
        documents.read_markdown(&root.join("inbox"), Kind::Inbox)?;
        documents.build(timezone)
    }

    pub fn timezone(&self) -> Tz {
        self.timezone
    }

    /// Reminder definitions whose optional Task or WorkUnit is still active.
    pub fn reminders(&self) -> impl Iterator<Item = &Document<Reminder>> {
        self.reminders.values().filter(|doc| {
            doc.data.target_id.is_none_or(|id| {
                if let Some(task) = self.tasks.get(&id) {
                    !matches!(
                        task.data.status,
                        TaskStatus::Completed | TaskStatus::Cancelled | TaskStatus::Archived
                    )
                } else {
                    self.units.get(&id).is_some_and(|unit| {
                        !matches!(
                            unit.data.status,
                            WorkUnitStatus::Completed | WorkUnitStatus::Cancelled
                        )
                    })
                }
            })
        })
    }

    pub fn calendar(&self) -> Vec<CalendarEntry<'_>> {
        let mut entries = Vec::new();
        for (id, doc) in &self.projects {
            append_plan(
                &mut entries,
                *id,
                &doc.title,
                doc.data.active_period,
                doc.data.deadline,
            );
        }
        for (id, doc) in &self.tasks {
            append_plan(
                &mut entries,
                *id,
                &doc.title,
                doc.data.active_period,
                if doc.data.occurrence.is_some() {
                    None
                } else {
                    doc.data.deadline
                },
            );
            if let Some(occurrence) = doc.data.occurrence {
                entries.push(CalendarEntry::task_occurrence(
                    *id,
                    &doc.title,
                    occurrence.date,
                ));
            }
        }
        for (id, doc) in &self.units {
            append_plan(&mut entries, *id, &doc.title, None, doc.data.deadline);
            entries.extend(doc.data.time_blocks.iter().map(|block| {
                CalendarEntry::time_block(*id, &doc.title, block.when, block.availability)
            }));
        }
        for (id, doc) in &self.events {
            if doc.data.recurrence.is_some() {
                continue;
            }
            entries.push(CalendarEntry::event(
                *id,
                &doc.title,
                doc.data.when,
                doc.data.availability,
            ));
        }
        entries
    }

    /// Project calendar entries intersecting an inclusive date window of at most 366 days.
    pub fn calendar_between(&self, window: DateSpan) -> Result<Vec<CalendarEntry<'_>>> {
        ensure!(
            (window.through() - window.start()).num_days() <= 366,
            "calendar query window must span no more than 366 days"
        );
        let mut entries: Vec<_> = self
            .calendar()
            .into_iter()
            .filter(|entry| entry.in_range(window, self.timezone))
            .collect();
        for (id, doc) in &self.events {
            let Some(rule) = doc.data.recurrence else {
                continue;
            };
            let Span::DateSpan(template) = doc.data.when else {
                bail!(
                    "{}: recurring events currently require a date-span template",
                    doc.path.display()
                );
            };
            let duration_days = (template.through() - template.start()).num_days();
            for day in rule.occurrence_dates(window)? {
                let through = day
                    .checked_add_days(Days::new(duration_days as u64))
                    .context("recurring event end date is out of range")?;
                let occurrence = DateSpan::new(day, through)?;
                if occurrence.overlaps(window) {
                    entries.push(CalendarEntry::event(
                        *id,
                        &doc.title,
                        Span::DateSpan(occurrence),
                        doc.data.availability,
                    ));
                }
            }
        }
        Ok(entries)
    }

    pub fn outline(&self) -> Vec<OutlineRow<'_>> {
        let mut tasks = BTreeMap::<Id, Vec<_>>::new();
        let mut units = BTreeMap::<Id, Vec<_>>::new();
        for doc in self.tasks.values() {
            tasks.entry(doc.data.project_id).or_default().push(doc);
        }
        for doc in self.units.values() {
            units.entry(doc.data.parent_task_id).or_default().push(doc);
        }
        let mut rows = Vec::new();
        for project in self.projects.values() {
            rows.push(row(project, project.data.id, 0, false));
            for task in tasks.remove(&project.data.id).into_iter().flatten() {
                rows.push(row(task, task.data.id, 1, false));
                for unit in units.remove(&task.data.id).into_iter().flatten() {
                    rows.push(row(unit, unit.data.id, 2, self.graph.blocked(unit.data.id)));
                }
            }
        }
        rows
    }

    pub fn source(&self, id: Id) -> Option<&Path> {
        self.projects
            .get(&id)
            .map(|doc| doc.path.as_path())
            .or_else(|| self.tasks.get(&id).map(|doc| doc.path.as_path()))
            .or_else(|| self.units.get(&id).map(|doc| doc.path.as_path()))
            .or_else(|| self.events.get(&id).map(|doc| doc.path.as_path()))
            .or_else(|| self.reminders.get(&id).map(|doc| doc.path.as_path()))
            .or_else(|| self.inbox.get(&id).map(|doc| doc.path.as_path()))
    }

    /// Inbox entries in stable UUID order.
    pub fn inbox_entries(&self) -> impl Iterator<Item = &Document<InboxEntry>> {
        self.inbox.values()
    }

    /// The exact document revision observed when this snapshot was loaded.
    pub fn revision(&self, id: Id) -> Option<&Revision> {
        self.revisions.get(&id)
    }

    pub fn is_work_unit(&self, id: Id) -> bool {
        self.units.contains_key(&id)
    }

    pub fn is_reminder_target(&self, id: Id) -> bool {
        self.tasks.contains_key(&id) || self.units.contains_key(&id)
    }

    pub fn work_unit(&self, id: Id) -> Option<&Document<WorkUnit>> {
        self.units.get(&id)
    }

    pub fn work_units(&self) -> impl Iterator<Item = &Document<WorkUnit>> {
        self.units.values()
    }

    /// Deadline of a Project, Task or WorkUnit; outer `None` means the ID is
    /// not a deadline-capable document.
    pub fn deadline(&self, id: Id) -> Option<Option<mcbtodo_domain::time::Deadline>> {
        self.projects
            .get(&id)
            .map(|doc| doc.data.deadline)
            .or_else(|| self.tasks.get(&id).map(|doc| doc.data.deadline))
            .or_else(|| self.units.get(&id).map(|doc| doc.data.deadline))
    }

    /// The validated WorkUnit dependency graph for this snapshot.
    pub fn task_graph(&self) -> &TaskGraph {
        &self.graph
    }

    pub fn graph_mermaid(&self) -> String {
        let mut output = String::from("flowchart LR\n");
        for &id in self.graph.order() {
            let title = mermaid_label(&self.units[&id].title);
            writeln!(output, "  u{}[\"{}\"]", id.simple(), title).expect("writing to String");
        }
        for (before, after) in self.graph.edges() {
            writeln!(output, "  u{} --> u{}", before.simple(), after.simple())
                .expect("writing to String");
        }
        output
    }
}

fn append_plan<'a>(
    entries: &mut Vec<CalendarEntry<'a>>,
    id: Id,
    title: &'a str,
    active: Option<mcbtodo_domain::time::Span>,
    deadline: Option<mcbtodo_domain::time::Deadline>,
) {
    if let Some(span) = active {
        entries.push(CalendarEntry::active(id, title, span));
    }
    if let Some(deadline) = deadline {
        entries.push(CalendarEntry::deadline(id, title, deadline));
    }
}

fn row<T>(doc: &Document<T>, id: Id, depth: usize, blocked: bool) -> OutlineRow<'_> {
    OutlineRow {
        id,
        depth,
        title: &doc.title,
        path: &doc.path,
        blocked,
    }
}

fn mermaid_label(title: &str) -> String {
    let mut result = String::new();
    for character in title.chars() {
        if character.is_alphanumeric() || character == ' ' {
            result.push(character);
        } else {
            write!(result, "#{};", u32::from(character)).expect("writing to String");
        }
    }
    result
}

#[derive(Default)]
struct Documents {
    paths: BTreeMap<Id, PathBuf>,
    revisions: BTreeMap<Id, Revision>,
    projects: BTreeMap<Id, Document<Project>>,
    tasks: BTreeMap<Id, Document<Task>>,
    units: BTreeMap<Id, Document<WorkUnit>>,
    events: BTreeMap<Id, Document<CalendarEvent>>,
    reminders: BTreeMap<Id, Document<Reminder>>,
    inbox: BTreeMap<Id, Document<InboxEntry>>,
}

impl Documents {
    fn read_markdown(&mut self, dir: &Path, kind: Kind) -> Result<()> {
        for path in entries(dir)? {
            if path.extension().is_some_and(|extension| extension == "md") {
                self.read(&path, kind)?;
            }
        }
        Ok(())
    }

    fn read(&mut self, path: &Path, expected: Kind) -> Result<()> {
        ensure!(
            is_kind(path, fs::FileType::is_file)?,
            "expected a file at {}",
            path.display()
        );
        let source =
            fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let (title, metadata) =
            document::parse(&source).with_context(|| format!("parsing {}", path.display()))?;
        let (id, kind) = match &metadata {
            Metadata::Project(data) => (data.id, Kind::Project),
            Metadata::Task(data) => (data.id, Kind::Task),
            Metadata::Unit(data) => (data.id, Kind::Unit),
            Metadata::Event(data) => (data.id, Kind::Event),
            Metadata::Reminder(data) => (data.id, Kind::Reminder),
            Metadata::Inbox(data) => (data.id, Kind::Inbox),
        };
        ensure!(
            kind == expected,
            "{} has {kind:?} schema, expected {expected:?}",
            path.display()
        );
        if let Some(first) = self.paths.insert(id, path.to_owned()) {
            bail!(
                "duplicate ID {id} in {} and {}",
                first.display(),
                path.display()
            );
        }
        self.revisions.insert(id, Revision::from_source(source));
        let path = path.to_owned();
        match metadata {
            Metadata::Project(data) => {
                self.projects.insert(id, Document { path, title, data });
            }
            Metadata::Task(data) => {
                self.tasks.insert(id, Document { path, title, data });
            }
            Metadata::Unit(data) => {
                self.units.insert(id, Document { path, title, data });
            }
            Metadata::Event(data) => {
                self.events.insert(id, Document { path, title, data });
            }
            Metadata::Reminder(data) => {
                self.reminders.insert(id, Document { path, title, data });
            }
            Metadata::Inbox(data) => {
                self.inbox.insert(id, Document { path, title, data });
            }
        }
        Ok(())
    }

    fn build(self, timezone: Tz) -> Result<Vault> {
        for entry in self.inbox.values() {
            if let Some(project_id) = entry.data.project_id {
                ensure!(
                    self.projects.contains_key(&project_id),
                    "{} references missing project {project_id}",
                    entry.path.display()
                );
            }
        }
        for event in self.events.values() {
            if let Some(rule) = event.data.recurrence {
                let Span::DateSpan(span) = event.data.when else {
                    bail!(
                        "{}: recurring events require a date-span template",
                        event.path.display()
                    );
                };
                ensure!(
                    rule.start() == span.start(),
                    "{} recurrence start must match the event's start date",
                    event.path.display()
                );
            }
        }
        for task in self.tasks.values() {
            ensure!(
                self.projects.contains_key(&task.data.project_id),
                "{} references missing project {}",
                task.path.display(),
                task.data.project_id
            );
        }
        for task in self.tasks.values() {
            match (task.data.recurrence, task.data.occurrence) {
                (Some(_), None) => {}
                (None, Some(occurrence)) => {
                    let template = self.tasks.get(&occurrence.template_id).with_context(|| {
                        format!(
                            "{} references missing recurring Task template {}",
                            task.path.display(),
                            occurrence.template_id
                        )
                    })?;
                    ensure!(
                        template.data.recurrence.is_some(),
                        "{} references a Task without a recurrence rule",
                        task.path.display()
                    );
                    let rule = template.data.recurrence.expect("checked above");
                    let occurrence_window = DateSpan::new(occurrence.date, occurrence.date)?;
                    ensure!(
                        rule.occurrence_dates(occurrence_window)?
                            .contains(&occurrence.date),
                        "{} date is not included by its recurring Task template",
                        task.path.display()
                    );
                    ensure!(
                        occurrence.id() == task.data.id,
                        "{} has a Task occurrence ID that does not match its template and date",
                        task.path.display()
                    );
                    ensure!(
                        task.data.project_id == template.data.project_id,
                        "{} and its recurring Task template belong to different projects",
                        task.path.display()
                    );
                }
                (None, None) => {}
                (Some(_), Some(_)) => bail!(
                    "{} cannot be both a recurring Task template and an occurrence",
                    task.path.display()
                ),
            }
        }
        for unit in self.units.values() {
            let parent = self.tasks.get(&unit.data.parent_task_id).with_context(|| {
                format!(
                    "{} references missing task {}",
                    unit.path.display(),
                    unit.data.parent_task_id
                )
            })?;
            ensure!(
                parent.data.project_id == unit.data.project_id,
                "{} has a different project from its parent task {}",
                unit.path.display(),
                parent.path.display()
            );
        }
        for reminder in self.reminders.values() {
            if let Some(target_id) = reminder.data.target_id {
                ensure!(
                    self.tasks.contains_key(&target_id) || self.units.contains_key(&target_id),
                    "{} references missing Task or WorkUnit {target_id}",
                    reminder.path.display()
                );
            }
        }
        let graph =
            TaskGraph::build(self.units.values().map(|doc| &doc.data)).map_err(|error| {
                let id = match error {
                    GraphError::DuplicateUnit(id)
                    | GraphError::Cycle(id)
                    | GraphError::MissingOverride(id) => id,
                    GraphError::Missing { unit, .. } | GraphError::DuplicateEdge { unit, .. } => {
                        unit
                    }
                };
                anyhow::Error::new(error).context(self.paths[&id].display().to_string())
            })?;
        Ok(Vault {
            timezone,
            projects: self.projects,
            tasks: self.tasks,
            units: self.units,
            events: self.events,
            reminders: self.reminders,
            inbox: self.inbox,
            graph,
            revisions: self.revisions,
        })
    }
}

fn entries(dir: &Path) -> Result<Vec<PathBuf>> {
    let metadata = match fs::symlink_metadata(dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).with_context(|| format!("reading {}", dir.display())),
    };
    ensure!(metadata.is_dir(), "expected directory at {}", dir.display());
    let mut paths = fs::read_dir(dir)
        .and_then(|entries| {
            entries
                .map(|entry| entry.map(|entry| entry.path()))
                .collect::<io::Result<Vec<_>>>()
        })
        .with_context(|| format!("listing {}", dir.display()))?;
    paths.sort_unstable();
    Ok(paths)
}

fn is_kind(path: &Path, check: impl FnOnce(&fs::FileType) -> bool) -> Result<bool> {
    let kind = fs::symlink_metadata(path)
        .with_context(|| format!("reading {}", path.display()))?
        .file_type();
    ensure!(
        !kind.is_symlink(),
        "symbolic links are unsupported at {}",
        path.display()
    );
    Ok(check(&kind))
}
