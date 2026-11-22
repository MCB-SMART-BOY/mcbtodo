use std::{fs, path::PathBuf, sync::Barrier, thread};

use chrono_tz::UTC;
use mcbtodo_domain::{
    Availability, CalendarEvent, CalendarKind, EvidenceRef, Id, InboxEntry, Priority, Task,
    TaskStatus, TimeBlock, WorkUnit, WorkUnitStatus, WorkflowRevisionHash,
    recurrence::Reminder,
    time::{DateSpan, DateTimeSpan, Deadline, Span},
};
use mcbtodo_vault::{
    BindWorkflow, CaptureInboxEntry, CaptureTask, CompleteWorkUnit, CreateEvent, CreateReminder,
    CreateTimeBlock, CreateWorkUnit, MaterializeTaskOccurrences, SetDeadline,
    SetWorkUnitDependencies, TriageInboxEntry, Vault, VaultStore, WriteError,
};
use tempfile::TempDir;

const PROJECT: Id = Id::from_u128(1);
const TASK: Id = Id::from_u128(2);
const UNIT: Id = Id::from_u128(3);
const PREREQUISITE: Id = Id::from_u128(4);
const CAPTURED: Id = Id::from_u128(5);
const EVENT: Id = Id::from_u128(6);
const REMINDER: Id = Id::from_u128(7);
const INBOX: Id = Id::from_u128(8);

fn day(value: &str) -> chrono::NaiveDate {
    value.parse().unwrap()
}

struct Fixture {
    root: TempDir,
    project: PathBuf,
    unit: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("projects/existing-project");
        fs::create_dir_all(dir.join("tasks")).unwrap();
        fs::create_dir_all(dir.join("units")).unwrap();
        let project = dir.join("project.md");
        fs::write(
            &project,
            format!("---\nschema: mcbtodo.project/v1\nid: {PROJECT}\n---\n\n# Project\n"),
        )
        .unwrap();
        fs::write(
            dir.join("tasks/existing-task.md"),
            format!(
                "---\nschema: mcbtodo.task/v1\nid: {TASK}\nproject_id: {PROJECT}\n---\n\n# Task\n"
            ),
        )
        .unwrap();
        Self {
            root,
            project,
            unit: dir.join("units/unit.md"),
        }
    }

    fn store(&self) -> VaultStore {
        VaultStore::new(self.root.path(), UTC)
    }

    fn snapshot(&self) -> Vault {
        Vault::load(self.root.path(), UTC).unwrap()
    }

    fn capture(&self) -> CaptureTask {
        CaptureTask {
            task: Task {
                id: CAPTURED,
                project_id: PROJECT,
                status: TaskStatus::Todo,
                priority: Priority::High,
                active_period: None,
                deadline: None,
                recurrence: None,
                occurrence: None,
                tags: vec!["design: revision".into()],
            },
            title: "Task: [draft]".into(),
            body: "## Notes\n\n```yaml\n---\n```\n".into(),
            expected_revision: self.snapshot().revision(PROJECT).unwrap().clone(),
        }
    }

    fn complete(&self) -> CompleteWorkUnit {
        CompleteWorkUnit {
            id: UNIT,
            expected_revision: self.snapshot().revision(UNIT).unwrap().clone(),
        }
    }

    fn time_block(&self) -> CreateTimeBlock {
        CreateTimeBlock {
            work_unit_id: UNIT,
            block: TimeBlock {
                when: DateTimeSpan::new(
                    "2026-11-20T09:00:00Z".parse().unwrap(),
                    "2026-11-20T11:00:00Z".parse().unwrap(),
                    UTC,
                )
                .unwrap(),
                availability: Availability::Free,
            },
            expected_revision: self.snapshot().revision(UNIT).unwrap().clone(),
        }
    }

    fn new_unit(&self, depends_on: Vec<Id>) -> CreateWorkUnit {
        CreateWorkUnit {
            unit: WorkUnit {
                id: UNIT,
                project_id: PROJECT,
                parent_task_id: TASK,
                status: Default::default(),
                priority: Priority::High,
                depends_on,
                estimated_minutes: Some(120),
                deadline: None,
                time_blocks: Vec::new(),
                workflow_revision: None,
                evidence: Vec::new(),
                tags: Vec::new(),
            },
            title: "Map pages".into(),
            body: "## Acceptance criteria\n\n- [ ] Tests pass\n".into(),
            expected_revision: self.snapshot().revision(TASK).unwrap().clone(),
        }
    }

    fn set_unit(&self, id: Id, metadata: &str) -> String {
        let source = format!(
            "---\nschema: mcbtodo.work-unit/v1\nid: {id}\nproject_id: {PROJECT}\nparent_task_id: {TASK}\n{metadata}---\n\n# Unit\n\nBody\n"
        );
        let path = if id == UNIT {
            self.unit.clone()
        } else {
            self.unit.with_file_name("prerequisite.md")
        };
        fs::write(path, &source).unwrap();
        source
    }
}

#[test]
fn time_block_preserves_existing_blocks_comments_and_crlf_body() {
    let fixture = Fixture::new();
    let source = fixture.set_unit(
        UNIT,
        "time_blocks: # preserve\n  - when:\n      start: '2026-11-20T07:00:00Z'\n      end: '2026-11-20T08:00:00Z'\n      timezone: UTC\n    extra: keep # metadata\n# before custom\ncustom: {theme: blue}\n",
    );
    let source = source.replace('\n', "\r\n");
    fs::write(&fixture.unit, &source).unwrap();
    fixture
        .store()
        .create_time_block(fixture.time_block())
        .unwrap();
    let after = fs::read_to_string(&fixture.unit).unwrap();
    let new_line = after
        .lines()
        .find(|line| line.contains("09:00:00"))
        .unwrap();
    assert!(new_line.starts_with("  - "));
    assert_eq!(after.replace(&format!("{new_line}\r\n"), ""), source);
    let snapshot = fixture.snapshot();
    let blocks: Vec<_> = snapshot
        .calendar()
        .into_iter()
        .filter(|entry| entry.id == UNIT && entry.kind == CalendarKind::TimeBlock)
        .collect();
    assert_eq!(blocks.len(), 2);
    assert!(blocks[0].busy);
    assert!(!blocks[1].busy);
}

#[test]
fn deadline_updates_and_clears_project_task_and_work_unit_with_revision_checks() {
    let fixture = Fixture::new();
    fixture
        .store()
        .create_work_unit(fixture.new_unit(Vec::new()))
        .unwrap();
    let due = Deadline::Date {
        value: "2026-12-01".parse().unwrap(),
    };
    for id in [PROJECT, TASK, UNIT] {
        let snapshot = fixture.snapshot();
        fixture
            .store()
            .set_deadline(SetDeadline {
                id,
                deadline: Some(due),
                expected_revision: snapshot.revision(id).unwrap().clone(),
            })
            .unwrap();
        assert_eq!(fixture.snapshot().deadline(id), Some(Some(due)));
    }

    let stale = fixture.snapshot();
    fixture
        .store()
        .set_deadline(SetDeadline {
            id: UNIT,
            deadline: None,
            expected_revision: stale.revision(UNIT).unwrap().clone(),
        })
        .unwrap();
    assert!(matches!(
        fixture.store().set_deadline(SetDeadline {
            id: UNIT,
            deadline: Some(due),
            expected_revision: stale.revision(UNIT).unwrap().clone(),
        }),
        Err(WriteError::StaleRevision { id: UNIT })
    ));
    assert_eq!(fixture.snapshot().deadline(UNIT), Some(None));
}

#[test]
fn setting_the_existing_deadline_is_a_byte_exact_noop() {
    let fixture = Fixture::new();
    let source = format!(
        "---\nschema: mcbtodo.project/v1\nid: {PROJECT}\ndeadline: {{kind: date, value: '2026-12-01'}} # retain\ncustom: keep\n---\n\n# Project\n\nBody\n"
    );
    fs::write(&fixture.project, &source).unwrap();
    let snapshot = fixture.snapshot();
    let deadline = Deadline::Date {
        value: "2026-12-01".parse().unwrap(),
    };
    fixture
        .store()
        .set_deadline(SetDeadline {
            id: PROJECT,
            deadline: Some(deadline),
            expected_revision: snapshot.revision(PROJECT).unwrap().clone(),
        })
        .unwrap();
    assert_eq!(fs::read_to_string(fixture.project).unwrap(), source);
}

#[test]
fn time_block_adds_missing_and_appends_to_existing_flow_sequences() {
    for metadata in [
        "custom: [one, two] # preserve\n",
        "time_blocks: [] # preserve\n",
        concat!(
            "time_blocks: [{when: {start: '2026-11-20T07:00:00Z', ",
            "end: '2026-11-20T08:00:00Z', timezone: UTC}}, # existing\n",
            "] # preserve\n",
        ),
    ] {
        let fixture = Fixture::new();
        let source = fixture.set_unit(UNIT, metadata);
        let request = fixture.time_block();
        let before = fixture.snapshot().calendar().len();
        let result = fixture.store().create_time_block(request).unwrap();
        assert_eq!(fixture.snapshot().calendar().len(), before + 1);
        let after = fs::read_to_string(&result.path).unwrap();
        assert!(after.contains("# preserve"));
        assert_eq!(
            after.split_once("\n---\n").unwrap().1,
            source.split_once("\n---\n").unwrap().1
        );
        if metadata.contains("# existing") {
            assert!(after.contains("# existing"));
        }
    }
}

#[test]
fn create_event_publishes_a_valid_calendar_document_atomically() {
    let fixture = Fixture::new();
    let request = CreateEvent {
        event: CalendarEvent {
            id: EVENT,
            when: Span::DateSpan(
                DateSpan::new("2026-11-20".parse().unwrap(), "2026-11-21".parse().unwrap())
                    .unwrap(),
            ),
            recurrence: None,
            availability: Availability::Busy,
            tags: Vec::new(),
        },
        title: "Release review".into(),
        body: "## Agenda\n".into(),
    };

    let persisted = fixture.store().create_event(request).unwrap();
    assert_eq!(persisted.id, EVENT);
    assert_eq!(
        persisted.path,
        fixture.root.path().join(format!("calendar/{EVENT}.md"))
    );
    let vault = fixture.snapshot();
    let entry = vault
        .calendar()
        .into_iter()
        .find(|entry| entry.id == EVENT)
        .unwrap();
    assert_eq!(entry.title, "Release review");
    assert_eq!(entry.kind, CalendarKind::Event);
    assert!(entry.busy);
    assert!(vault.revision(EVENT).is_some());
}

#[test]
fn create_events_atomic_publishes_all_events_and_preserves_existing_bytes() {
    let fixture = Fixture::new();
    let initial = fixture
        .store()
        .create_event(CreateEvent {
            event: CalendarEvent {
                id: EVENT,
                when: Span::DateSpan(DateSpan::new(day("2026-11-20"), day("2026-11-20")).unwrap()),
                recurrence: None,
                availability: Availability::Busy,
                tags: Vec::new(),
            },
            title: "Existing".into(),
            body: "Keep this body.\n".into(),
        })
        .unwrap();
    let original = fs::read(initial.path).unwrap();
    let requests = [8_u128, 9]
        .into_iter()
        .map(|id| CreateEvent {
            event: CalendarEvent {
                id: Id::from_u128(id),
                when: Span::DateSpan(DateSpan::new(day("2026-11-21"), day("2026-11-21")).unwrap()),
                recurrence: None,
                availability: Availability::Busy,
                tags: Vec::new(),
            },
            title: format!("Imported {id}"),
            body: String::new(),
        })
        .collect();

    assert_eq!(fixture.store().create_events_atomic(requests).unwrap(), 2);
    assert_eq!(
        fs::read(fixture.root.path().join(format!("calendar/{EVENT}.md"))).unwrap(),
        original
    );
    let vault = fixture.snapshot();
    assert!(vault.revision(Id::from_u128(8)).is_some());
    assert!(vault.revision(Id::from_u128(9)).is_some());
}

#[test]
fn create_events_atomic_rejects_conflicts_before_publishing_anything() {
    let fixture = Fixture::new();
    let request = |id| CreateEvent {
        event: CalendarEvent {
            id,
            when: Span::DateSpan(DateSpan::new(day("2026-11-21"), day("2026-11-21")).unwrap()),
            recurrence: None,
            availability: Availability::Busy,
            tags: Vec::new(),
        },
        title: "Imported".into(),
        body: String::new(),
    };
    assert!(matches!(
        fixture
            .store()
            .create_events_atomic(vec![request(Id::from_u128(8)), request(Id::from_u128(8))]),
        Err(WriteError::DuplicateId { id }) if id == Id::from_u128(8)
    ));
    assert!(!fixture.root.path().join("calendar").exists());
    assert!(fixture.snapshot().calendar().is_empty());
}

#[test]
fn recurring_date_event_preserves_interval_and_full_spans_at_window_edges() {
    let fixture = Fixture::new();
    let event = format!(
        "---\nschema: mcbtodo.calendar-event/v1\nid: {EVENT}\nwhen:\n  kind: date_span\n  start: '2026-01-05'\n  through: '2026-01-06'\nrecurrence:\n  start: '2026-01-05'\n  through: '2026-02-28'\n  frequency: weekly\n  interval: 2\n  dst: {{gap: skip, fold: earlier}}\n---\n\n# Sprint planning\n"
    );
    let path = fixture.root.path().join("calendar/recurring.md");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, event).unwrap();
    let vault = fixture.snapshot();
    let window =
        DateSpan::new("2026-01-20".parse().unwrap(), "2026-02-28".parse().unwrap()).unwrap();

    let entries = vault.calendar_between(window).unwrap();
    let occurrences: Vec<_> = entries
        .iter()
        .filter(|entry| entry.id == EVENT && entry.kind == CalendarKind::Event)
        .map(|entry| entry.occurrence)
        .collect();
    assert_eq!(
        occurrences,
        [
            ("2026-01-19", "2026-01-20"),
            ("2026-02-02", "2026-02-03"),
            ("2026-02-16", "2026-02-17"),
        ]
        .map(
            |(start, end)| mcbtodo_domain::Occurrence::Span(Span::DateSpan(
                DateSpan::new(day(start), day(end)).unwrap(),
            ))
        )
    );
}

#[test]
fn recurring_date_event_includes_a_continuation_from_before_the_window() {
    let fixture = Fixture::new();
    let event = format!(
        "\
---
schema: mcbtodo.calendar-event/v1
id: {EVENT}
when: {{kind: date_span, start: '2026-11-29', through: '2026-12-01'}}
recurrence:
  start: '2026-11-29'
  through: '2026-11-29'
  frequency: weekly
  dst: {{gap: skip, fold: earlier}}
---

# Monthly review
"
    );
    let path = fixture.root.path().join("calendar/review.md");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, event).unwrap();
    let vault = fixture.snapshot();

    let dec1 = day("2026-12-01");
    let entries = vault
        .calendar_between(DateSpan::new(dec1, dec1).unwrap())
        .unwrap();
    let occurrences: Vec<_> = entries
        .iter()
        .filter(|entry| entry.id == EVENT)
        .map(|entry| entry.occurrence)
        .collect();
    assert_eq!(
        occurrences,
        [mcbtodo_domain::Occurrence::Span(Span::DateSpan(
            DateSpan::new(day("2026-11-29"), dec1).unwrap()
        ))]
    );
    let dec2 = day("2026-12-02");
    assert!(
        vault
            .calendar_between(DateSpan::new(dec2, dec2).unwrap())
            .unwrap()
            .iter()
            .all(|entry| entry.id != EVENT)
    );
}

#[test]
fn recurring_task_occurrences_have_stable_ids_and_retry_without_resetting_state() {
    let fixture = Fixture::new();
    let template = format!(
        "---\nschema: mcbtodo.task/v1\nid: {CAPTURED}\nproject_id: {PROJECT}\nrecurrence:\n  start: '2026-11-20'\n  through: '2026-11-30'\n  frequency: weekly\n  interval: 1\n  dst: {{gap: skip, fold: earlier}}\n---\n\n# Weekly review\n\nKeep these notes on each occurrence.\n"
    );
    fs::create_dir_all(fixture.root.path().join("projects/existing-project/tasks")).unwrap();
    let path = fixture
        .root
        .path()
        .join(format!("projects/existing-project/tasks/{CAPTURED}.md"));
    fs::write(&path, template).unwrap();
    let vault = fixture.snapshot();
    let request = || MaterializeTaskOccurrences {
        template_id: CAPTURED,
        window: DateSpan::new(day("2026-11-20"), day("2026-11-30")).unwrap(),
        expected_revision: vault.revision(CAPTURED).unwrap().clone(),
    };

    let ids = fixture
        .store()
        .materialize_task_occurrences(request())
        .unwrap();
    assert_eq!(ids.len(), 2);
    assert_eq!(
        ids,
        fixture
            .store()
            .materialize_task_occurrences(request())
            .unwrap()
    );
    let first_date = day("2026-11-20");
    let first_id = mcbtodo_domain::TaskOccurrence {
        template_id: CAPTURED,
        date: first_date,
    }
    .id();
    let instance_path = fixture
        .root
        .path()
        .join(format!("projects/existing-project/tasks/{first_id}.md"));
    let original = fs::read_to_string(&instance_path).unwrap();
    assert!(original.contains("Keep these notes on each occurrence."));
    fs::write(
        &instance_path,
        original.replace("status: todo", "status: completed"),
    )
    .unwrap();
    fixture
        .store()
        .materialize_task_occurrences(request())
        .unwrap();
    assert!(
        fs::read_to_string(instance_path)
            .unwrap()
            .contains("status: completed")
    );
    let vault = fixture.snapshot();
    assert!(
        vault
            .calendar()
            .iter()
            .any(|entry| { entry.id == first_id && entry.kind == CalendarKind::TaskOccurrence })
    );
}

#[test]
fn vault_rejects_a_recurring_task_occurrence_with_an_unstable_id() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.root.path().join("projects/existing-project/tasks")).unwrap();
    fs::write(
        fixture.root.path().join(format!("projects/existing-project/tasks/{CAPTURED}.md")),
        format!(
            "---\nschema: mcbtodo.task/v1\nid: {CAPTURED}\nproject_id: {PROJECT}\nrecurrence:\n  start: '2026-11-20'\n  frequency: weekly\n  interval: 1\n  dst: {{gap: skip, fold: earlier}}\n---\n\n# Weekly review\n"
        ),
    )
    .unwrap();
    fs::write(
        fixture
            .root
            .path()
            .join("projects/existing-project/tasks/8.md"),
        format!(
            "---\nschema: mcbtodo.task/v1\nid: {}\nproject_id: {PROJECT}\noccurrence:\n  template_id: {CAPTURED}\n  date: '2026-11-20'\n---\n\n# Forged occurrence\n",
            Id::from_u128(8)
        ),
    )
    .unwrap();

    let error = Vault::load(fixture.root.path(), UTC).unwrap_err();
    assert!(format!("{error:#}").contains("does not match its template and date"));
}

#[test]
fn vault_rejects_recurring_events_with_mismatched_anchors() {
    for (when, start) in [
        (
            "kind: date_span\n  start: '2026-01-05'\n  through: '2026-01-06'",
            "2026-01-06",
        ),
        (
            "kind: date_time_span\n  start: '2026-01-05T09:00:00+08:00'\n  end: '2026-01-05T10:00:00+08:00'\n  timezone: Asia/Shanghai",
            "2026-01-06",
        ),
    ] {
        let fixture = Fixture::new();
        let event = format!(
            "---\nschema: mcbtodo.calendar-event/v1\nid: {EVENT}\nwhen:\n  {when}\nrecurrence:\n  start: '{start}'\n  frequency: weekly\n  dst: {{gap: skip, fold: earlier}}\n---\n\n# Recurring event\n"
        );
        let path = fixture.root.path().join("calendar/recurring.md");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, event).unwrap();

        let error = Vault::load(fixture.root.path(), UTC).unwrap_err();
        let diagnostic = format!("{error:#}");
        assert!(diagnostic.contains("calendar/recurring.md"), "{diagnostic}");
        assert!(
            diagnostic.contains("recurrence start must match"),
            "{diagnostic}"
        );
    }
}

#[test]
fn create_event_rejects_a_duplicate_uuid_without_replacing_bytes() {
    let fixture = Fixture::new();
    let request = || CreateEvent {
        event: CalendarEvent {
            id: EVENT,
            when: Span::DateSpan(
                DateSpan::new("2026-11-20".parse().unwrap(), "2026-11-20".parse().unwrap())
                    .unwrap(),
            ),
            recurrence: None,
            availability: Availability::Busy,
            tags: Vec::new(),
        },
        title: "Release review".into(),
        body: String::new(),
    };
    let persisted = fixture.store().create_event(request()).unwrap();
    let before = fs::read(&persisted.path).unwrap();

    assert!(matches!(
        fixture.store().create_event(request()),
        Err(WriteError::DuplicateId { id: EVENT })
    ));
    assert_eq!(fs::read(persisted.path).unwrap(), before);
}

#[test]
fn create_reminder_publishes_valid_targeted_markdown_and_rejects_bad_ids_or_targets() {
    let fixture = Fixture::new();
    let schedule = serde_json::from_str(
        r#"{"kind":"once","at":{"value":"2026-11-20T09:00:00+00:00","timezone":"UTC"}}"#,
    )
    .unwrap();
    let request = || CreateReminder {
        reminder: Reminder {
            id: REMINDER,
            target_id: Some(TASK),
            schedule,
        },
        title: "Review design".into(),
        body: "Check the implementation notes.".into(),
    };
    let persisted = fixture.store().create_reminder(request()).unwrap();
    assert_eq!(
        persisted.path,
        fixture.root.path().join(format!("reminders/{REMINDER}.md"))
    );
    let source = fs::read_to_string(&persisted.path).unwrap();
    assert!(source.contains("schema: mcbtodo.reminder/v1"));
    assert!(source.contains(&format!("target_id: {TASK}")));
    let vault = fixture.snapshot();
    assert!(vault.source(REMINDER).is_some());

    assert!(matches!(
        fixture.store().create_reminder(request()),
        Err(WriteError::DuplicateId { id: REMINDER })
    ));

    let invalid_target = CreateReminder {
        reminder: Reminder {
            id: Id::from_u128(8),
            target_id: Some(Id::from_u128(99)),
            schedule,
        },
        title: "Invalid".into(),
        body: String::new(),
    };
    assert!(matches!(
        fixture.store().create_reminder(invalid_target),
        Err(WriteError::ReminderTargetNotFound { id }) if id == Id::from_u128(99)
    ));
}

#[test]
fn create_reminder_allows_an_unassociated_reminder() {
    let fixture = Fixture::new();
    let schedule = serde_json::from_str(
        r#"{"kind":"recurring","rule":{"start":"2026-11-20","time":"09:00:00","timezone":"UTC","frequency":"weekly","interval":2,"dst":{"gap":"skip","fold":"earlier"}}}"#,
    )
    .unwrap();
    fixture
        .store()
        .create_reminder(CreateReminder {
            reminder: Reminder {
                id: REMINDER,
                target_id: None,
                schedule,
            },
            title: "Weekly review".into(),
            body: String::new(),
        })
        .unwrap();
    assert_eq!(fixture.snapshot().reminders().count(), 1);
}

#[test]
fn time_block_rejects_stale_edits_and_shared_sequence_anchors() {
    let fixture = Fixture::new();
    fixture.set_unit(UNIT, "");
    let request = fixture.time_block();
    let edited = fixture.set_unit(UNIT, "custom: edited\n");
    assert!(matches!(
        fixture.store().create_time_block(request),
        Err(WriteError::StaleRevision { id: UNIT })
    ));
    assert_eq!(fs::read_to_string(&fixture.unit).unwrap(), edited);
    let anchored = fixture.set_unit(UNIT, "time_blocks: &blocks []\ncustom: *blocks\n");
    assert!(matches!(
        fixture.store().create_time_block(fixture.time_block()),
        Err(WriteError::InvalidDocument(_))
    ));
    assert_eq!(fs::read_to_string(&fixture.unit).unwrap(), anchored);
}

#[test]
fn time_block_checks_the_current_dag_before_writing() {
    let fixture = Fixture::new();
    fixture.set_unit(UNIT, "");
    let request = fixture.time_block();
    let source = fs::read_to_string(&fixture.unit).unwrap();
    fixture.set_unit(PREREQUISITE, &format!("depends_on: [{PREREQUISITE}]\n"));
    assert!(matches!(
        fixture.store().create_time_block(request),
        Err(WriteError::InvalidVault(_))
    ));
    assert_eq!(fs::read_to_string(&fixture.unit).unwrap(), source);
}

#[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
#[test]
fn create_project_checks_uuid_and_directory_without_overwriting() {
    use mcbtodo_domain::Project;
    use mcbtodo_vault::CreateProject;

    let root = tempfile::tempdir().unwrap();
    let store = VaultStore::new(root.path(), UTC);
    let create = |id, slug: &str| CreateProject {
        project: Project {
            id,
            status: Default::default(),
            active_period: None,
            deadline: None,
            tags: vec!["new".into()],
        },
        slug: Some(slug.to_owned()),
        title: "Rust OS".into(),
        body: "## Outcome\n\nRun a kernel.\n".into(),
    };
    assert!(matches!(
        store.create_project(create(PROJECT, "../outside")),
        Err(WriteError::InvalidDocument(_))
    ));
    let document = store.create_project(create(PROJECT, "rust-os")).unwrap();
    let snapshot = Vault::load(root.path(), UTC).unwrap();
    assert!(document.path.ends_with("projects/rust-os/project.md"));
    assert_eq!(snapshot.revision(PROJECT), Some(&document.revision));
    assert_eq!(snapshot.outline()[0].title, "Rust OS");
    let original = fs::read(&document.path).unwrap();
    assert!(matches!(
        store.create_project(create(PROJECT, "other")),
        Err(WriteError::DuplicateId { id: PROJECT })
    ));
    assert!(matches!(
        store.create_project(create(TASK, "rust-os")),
        Err(WriteError::DestinationExists { .. })
    ));
    assert_eq!(fs::read(&document.path).unwrap(), original);
}

#[test]
fn create_work_unit_checks_parent_revision_and_candidate_dag() {
    let fixture = Fixture::new();
    fixture.set_unit(PREREQUISITE, "status: todo\n");
    for depends_on in [vec![CAPTURED], vec![UNIT], vec![PREREQUISITE; 2]] {
        assert!(matches!(
            fixture
                .store()
                .create_work_unit(fixture.new_unit(depends_on)),
            Err(WriteError::InvalidDocument(_))
        ));
        assert_eq!(fixture.snapshot().source(UNIT), None);
    }
    let stale = fixture.new_unit(vec![PREREQUISITE]);
    let task = fixture.snapshot().source(TASK).unwrap().to_owned();
    let mut edited = fs::read_to_string(&task).unwrap();
    edited.push_str("\nTask notes\n");
    fs::write(task, edited).unwrap();
    assert!(matches!(
        fixture.store().create_work_unit(stale),
        Err(WriteError::StaleRevision { id: TASK })
    ));
    let result = fixture
        .store()
        .create_work_unit(fixture.new_unit(vec![PREREQUISITE]))
        .unwrap();
    assert!(result.path.ends_with(format!("units/{UNIT}.md")));
    let snapshot = fixture.snapshot();
    assert_eq!(snapshot.revision(UNIT), Some(&result.revision));
    let graph = snapshot.task_graph();
    assert_eq!(graph.order(), &[PREREQUISITE, UNIT]);
    assert_eq!(graph.edges().collect::<Vec<_>>(), [(PREREQUISITE, UNIT)]);
    assert!(
        snapshot
            .outline()
            .iter()
            .any(|row| row.id == UNIT && row.blocked)
    );
}

#[test]
fn capture_creates_a_typed_task_in_the_project_directory() {
    let fixture = Fixture::new();
    let project_before = fs::read(&fixture.project).unwrap();
    let result = fixture.store().capture_task(fixture.capture()).unwrap();
    assert_eq!(result.id, CAPTURED);
    assert_eq!(
        result.path,
        fixture
            .project
            .with_file_name(format!("tasks/{CAPTURED}.md"))
    );
    let content = fs::read_to_string(&result.path).unwrap();
    assert!(content.ends_with("# Task: [draft]\n\n## Notes\n\n```yaml\n---\n```\n"));
    assert!(content.contains("priority: high"));
    let snapshot = fixture.snapshot();
    assert_eq!(snapshot.revision(CAPTURED), Some(&result.revision));
    assert!(
        snapshot
            .outline()
            .iter()
            .any(|row| row.id == CAPTURED && row.depth == 1)
    );
    assert_eq!(fs::read(&fixture.project).unwrap(), project_before);
}

#[test]
fn capture_rejects_a_stale_project_revision() {
    let fixture = Fixture::new();
    let request = fixture.capture();
    let mut content = fs::read_to_string(&fixture.project).unwrap();
    content.push_str("\nExternally edited.\n");
    fs::write(&fixture.project, content).unwrap();
    assert!(matches!(
        fixture.store().capture_task(request),
        Err(WriteError::StaleRevision { id: PROJECT })
    ));
    assert!(fixture.snapshot().source(CAPTURED).is_none());
}

#[test]
fn inbox_capture_and_triage_preserve_content_and_use_revisions() {
    let fixture = Fixture::new();
    let captured = fixture
        .store()
        .capture_inbox_entry(CaptureInboxEntry {
            entry: InboxEntry {
                id: INBOX,
                captured_at: "2026-11-20T12:00:00Z".parse().unwrap(),
                project_id: None,
                tags: vec!["idea".into()],
            },
            title: "Unsorted idea".into(),
            body: "## Notes\n\nKeep this verbatim.\n".into(),
        })
        .unwrap();
    assert!(captured.path.ends_with(format!("inbox/{INBOX}.md")));
    let captured_source = fs::read(&captured.path).unwrap();
    let snapshot = fixture.snapshot();
    let triaged = fixture
        .store()
        .triage_inbox_entry(TriageInboxEntry {
            id: INBOX,
            project_id: PROJECT,
            expected_revision: snapshot.revision(INBOX).unwrap().clone(),
        })
        .unwrap();
    let after = fs::read_to_string(&triaged.path).unwrap();
    assert!(after.contains(&format!("project_id: {PROJECT}")));
    assert!(after.ends_with("Keep this verbatim.\n"));
    assert_eq!(
        fixture
            .snapshot()
            .inbox_entries()
            .next()
            .unwrap()
            .data
            .project_id,
        Some(PROJECT)
    );
    let task_id = fixture
        .snapshot()
        .inbox_entries()
        .next()
        .unwrap()
        .data
        .task_id();
    let task_path = fixture
        .project
        .parent()
        .unwrap()
        .join("tasks")
        .join(format!("{task_id}.md"));
    let task_source = fs::read_to_string(&task_path).unwrap();
    assert!(task_source.contains("schema: mcbtodo.task/v1"));
    assert!(task_source.ends_with("Keep this verbatim."));

    // Model a crash after Task publication but before Inbox metadata changed.
    fs::write(&captured.path, captured_source).unwrap();
    let retry_revision = fixture.snapshot().revision(INBOX).unwrap().clone();
    let retried = fixture
        .store()
        .triage_inbox_entry(TriageInboxEntry {
            id: INBOX,
            project_id: PROJECT,
            expected_revision: retry_revision,
        })
        .unwrap();
    assert_eq!(retried.revision, triaged.revision);
    assert_eq!(fs::read_to_string(task_path).unwrap(), task_source);

    let conflicting_revision = fixture.snapshot().revision(INBOX).unwrap().clone();
    let task_path = fixture
        .project
        .parent()
        .unwrap()
        .join("tasks")
        .join(format!("{task_id}.md"));
    fs::write(
        &task_path,
        task_source.replace("Keep this verbatim.", "Edited task"),
    )
    .unwrap();
    assert!(matches!(
        fixture.store().triage_inbox_entry(TriageInboxEntry {
            id: INBOX,
            project_id: PROJECT,
            expected_revision: conflicting_revision,
        }),
        Err(WriteError::DestinationExists { .. })
    ));
}

#[test]
fn capture_rejects_an_existing_destination_with_a_different_id() {
    let fixture = Fixture::new();
    let dest = fixture
        .project
        .with_file_name(format!("tasks/{CAPTURED}.md"));
    let original = format!(
        "---\nschema: mcbtodo.task/v1\nid: {PREREQUISITE}\nproject_id: {PROJECT}\n---\n\n# Unrelated task\n"
    );
    fs::write(&dest, &original).unwrap();
    assert!(matches!(
        fixture.store().capture_task(fixture.capture()),
        Err(WriteError::DestinationExists { .. })
    ));
    assert_eq!(fs::read_to_string(dest).unwrap(), original);
}

#[test]
fn complete_changes_only_status_and_unblocks_dependents() {
    let fixture = Fixture::new();
    let original = fixture.set_unit(
        UNIT,
        "# front matter note\n\"status\": 'in_progress' # preserve this\nextra:\n  status: custom\n  text: |\n    extra body\n",
    );
    let original = original.replace("# Unit", "# 单元  ").replace('\n', "\r\n");
    fs::write(&fixture.unit, &original).unwrap();
    fixture.set_unit(PREREQUISITE, &format!("depends_on: [{UNIT}]\n"));
    assert!(
        fixture
            .snapshot()
            .outline()
            .iter()
            .find(|row| row.id == PREREQUISITE)
            .unwrap()
            .blocked
    );
    let request = fixture.complete();
    let old = request.expected_revision.clone();
    let result = fixture.store().complete_work_unit(request).unwrap();
    assert_ne!(result.revision, old);
    assert_eq!(
        fs::read_to_string(&fixture.unit).unwrap(),
        original.replace("'in_progress'", "'completed'")
    );
    let snapshot = fixture.snapshot();
    assert_eq!(snapshot.revision(UNIT), Some(&result.revision));
    assert!(
        !snapshot
            .outline()
            .iter()
            .find(|row| row.id == PREREQUISITE)
            .unwrap()
            .blocked
    );
}

#[test]
fn complete_inserts_missing_status_without_editing_the_body() {
    let fixture = Fixture::new();
    let original = fixture.set_unit(UNIT, "extra: keep\n");
    fixture
        .store()
        .complete_work_unit(fixture.complete())
        .unwrap();
    let updated = fs::read_to_string(&fixture.unit).unwrap();
    assert_eq!(updated.matches("status: completed\n").count(), 1);
    assert_eq!(updated.replace("status: completed\n", ""), original);
}

#[test]
fn complete_rechecks_prerequisites_even_with_a_current_unit_revision() {
    let fixture = Fixture::new();
    fixture.set_unit(PREREQUISITE, "status: completed\n");
    let original = fixture.set_unit(UNIT, &format!("depends_on: [{PREREQUISITE}]\n"));
    let request = fixture.complete();
    fixture.set_unit(PREREQUISITE, "status: todo\n");
    assert!(matches!(
        fixture.store().complete_work_unit(request),
        Err(WriteError::Blocked { id: UNIT })
    ));
    assert_eq!(fs::read_to_string(&fixture.unit).unwrap(), original);
    assert!(
        fixture
            .store()
            .work_unit_completed_events()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn complete_rejects_a_new_cycle_before_writing() {
    let fixture = Fixture::new();
    fixture.set_unit(PREREQUISITE, "status: completed\n");
    let original = fixture.set_unit(UNIT, &format!("depends_on: [{PREREQUISITE}]\n"));
    let request = fixture.complete();
    fixture.set_unit(PREREQUISITE, &format!("depends_on: [{UNIT}]\n"));
    assert!(matches!(
        fixture.store().complete_work_unit(request),
        Err(WriteError::InvalidVault(_))
    ));
    assert_eq!(fs::read_to_string(&fixture.unit).unwrap(), original);
}

#[test]
fn complete_does_not_publish_when_the_event_store_is_unavailable() {
    let fixture = Fixture::new();
    let original = fixture.set_unit(UNIT, "status: todo\n");
    fs::create_dir(fixture.root.path().join(".mcbtodo-events.sqlite3")).unwrap();
    assert!(matches!(
        fixture.store().complete_work_unit(fixture.complete()),
        Err(WriteError::Storage(_))
    ));
    assert_eq!(fs::read_to_string(&fixture.unit).unwrap(), original);
}

#[test]
fn complete_does_not_overwrite_a_human_body_edit() {
    let fixture = Fixture::new();
    let original = fixture.set_unit(UNIT, "status: todo\n");
    let request = fixture.complete();
    let edited = format!("{original}\nHuman edit.\n");
    fs::write(&fixture.unit, &edited).unwrap();
    assert_ne!(
        request.expected_revision.digest(),
        fixture.snapshot().revision(UNIT).unwrap().digest()
    );
    assert!(matches!(
        fixture.store().complete_work_unit(request),
        Err(WriteError::StaleRevision { id: UNIT })
    ));
    assert_eq!(fs::read_to_string(&fixture.unit).unwrap(), edited);
    assert!(
        fixture
            .store()
            .work_unit_completed_events()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn workflow_binding_round_trips_with_stale_and_noop_checks() {
    let fixture = Fixture::new();
    let original = fixture.set_unit(
        UNIT,
        "workflow_revision: null # pinned\ncustom: {keep: yes}\n",
    );
    let hash: WorkflowRevisionHash = "1".repeat(64).parse().unwrap();
    let stale = fixture.complete();
    let request = |expected_revision| BindWorkflow {
        work_unit_id: UNIT,
        workflow_revision: hash.clone(),
        expected_revision,
    };
    let store = fixture.store();
    let written = store
        .bind_workflow(request(stale.expected_revision.clone()))
        .unwrap();
    assert_eq!(
        fs::read_to_string(&fixture.unit).unwrap(),
        original.replace("null # pinned", &format!("'{hash}' # pinned"))
    );
    assert_eq!(
        fixture
            .snapshot()
            .work_unit(UNIT)
            .unwrap()
            .data
            .workflow_revision,
        Some(hash.clone())
    );
    assert!(matches!(
        store.bind_workflow(request(stale.expected_revision)),
        Err(WriteError::StaleRevision { id: UNIT })
    ));
    assert_eq!(
        store
            .bind_workflow(request(written.revision.clone()))
            .unwrap()
            .revision,
        written.revision
    );
}

#[test]
fn vault_rejects_malformed_workflow_revisions_and_evidence() {
    let fixture = Fixture::new();
    for metadata in [
        "workflow_revision: not-a-published-hash\n",
        "evidence: [{run_id: no, step_index: 0}]\n",
        "evidence: [{run_id: 00000000-0000-4000-8000-000000000005, step_index: -1}]\n",
    ] {
        fixture.set_unit(UNIT, metadata);
        assert!(Vault::load(fixture.root.path(), UTC).is_err(), "{metadata}");
    }
}

#[test]
fn binding_an_empty_workflow_field_preserves_its_comment() {
    let fixture = Fixture::new();
    let original = fixture.set_unit(UNIT, "workflow_revision: # keep\n");
    let hash: WorkflowRevisionHash = "a".repeat(64).parse().unwrap();
    fixture
        .store()
        .bind_workflow(BindWorkflow {
            work_unit_id: UNIT,
            workflow_revision: hash.clone(),
            expected_revision: fixture.complete().expected_revision,
        })
        .unwrap();
    assert_eq!(
        fs::read_to_string(&fixture.unit).unwrap(),
        original.replace(
            "workflow_revision:",
            &format!("workflow_revision: '{hash}'")
        )
    );
}

#[test]
fn dependency_edit_preserves_crlf_comments_and_markdown_and_checks_revisions() {
    let fixture = Fixture::new();
    fixture.set_unit(PREREQUISITE, "status: completed\n");
    let original = fixture
        .set_unit(UNIT, "depends_on: [] # reviewed\ncustom: [keep, exact]\n")
        .replace('\n', "\r\n");
    fs::write(&fixture.unit, &original).unwrap();
    let expected_revision = fixture.snapshot().revision(UNIT).unwrap().clone();
    let store = fixture.store();
    store
        .set_work_unit_dependencies(SetWorkUnitDependencies {
            work_unit_id: UNIT,
            depends_on: vec![PREREQUISITE],
            expected_revision: expected_revision.clone(),
        })
        .unwrap();
    let updated = fs::read_to_string(&fixture.unit).unwrap();
    assert!(updated.contains(&format!("depends_on: [{PREREQUISITE}] # reviewed\r\n")));
    assert!(updated.ends_with("custom: [keep, exact]\r\n---\r\n\r\n# Unit\r\n\r\nBody\r\n"));
    assert_eq!(
        fixture.snapshot().work_unit(UNIT).unwrap().data.depends_on,
        [PREREQUISITE]
    );
    assert!(matches!(
        store.set_work_unit_dependencies(SetWorkUnitDependencies {
            work_unit_id: UNIT,
            depends_on: Vec::new(),
            expected_revision,
        }),
        Err(WriteError::StaleRevision { id: UNIT })
    ));
    assert_eq!(fs::read_to_string(&fixture.unit).unwrap(), updated);
}

#[test]
fn dependency_edit_rejects_invalid_edges_and_cycles_without_writing() {
    let fixture = Fixture::new();
    fixture.set_unit(PREREQUISITE, "status: todo\n");
    let original = fixture.set_unit(UNIT, "depends_on: []\n");
    let store = fixture.store();
    for depends_on in [
        vec![UNIT],
        vec![Id::from_u128(99)],
        vec![PREREQUISITE, PREREQUISITE],
    ] {
        let error = store
            .set_work_unit_dependencies(SetWorkUnitDependencies {
                work_unit_id: UNIT,
                depends_on,
                expected_revision: fixture.snapshot().revision(UNIT).unwrap().clone(),
            })
            .unwrap_err();
        assert!(matches!(error, WriteError::InvalidDocument(_)));
        assert_eq!(fs::read_to_string(&fixture.unit).unwrap(), original);
    }
    fixture.set_unit(PREREQUISITE, &format!("depends_on: [{UNIT}]\n"));
    let error = store
        .set_work_unit_dependencies(SetWorkUnitDependencies {
            work_unit_id: UNIT,
            depends_on: vec![PREREQUISITE],
            expected_revision: fixture.snapshot().revision(UNIT).unwrap().clone(),
        })
        .unwrap_err();
    assert!(matches!(error, WriteError::InvalidDocument(_)));
    assert_eq!(fs::read_to_string(&fixture.unit).unwrap(), original);
}

#[test]
fn complete_appends_evidence_once_and_preserves_existing_markdown() {
    let fixture = Fixture::new();
    let original = fixture
        .set_unit(
            UNIT,
            &format!(
                "status: 'todo' # keep\nevidence: # keep sequence\n  - run_id: {TASK}\n    step_index: 0 # keep old ref\ncustom: [one, two]\n"
            ),
        )
        .replace('\n', "\r\n");
    fs::write(&fixture.unit, &original).unwrap();
    let old = EvidenceRef {
        run_id: TASK,
        step_index: 0,
    };
    let new = EvidenceRef {
        run_id: CAPTURED,
        step_index: 2,
    };
    let store = fixture.store();
    let written = store
        .complete_work_unit_with_evidence(fixture.complete(), &[old, new, new])
        .unwrap();
    let snapshot = fixture.snapshot();
    let unit = &snapshot.work_unit(UNIT).unwrap().data;
    assert_eq!(unit.status, WorkUnitStatus::Completed);
    assert_eq!(unit.evidence, [old, new]);
    let updated = fs::read_to_string(&fixture.unit).unwrap();
    let added = updated
        .lines()
        .find(|line| line.contains(&CAPTURED.to_string()))
        .unwrap();
    assert_eq!(
        updated.replace(&format!("{added}\r\n"), ""),
        original.replace("'todo'", "'completed'")
    );
    assert_eq!(
        store
            .complete_work_unit_with_evidence(fixture.complete(), &[new])
            .unwrap()
            .revision,
        written.revision
    );
    let events = fixture.store().work_unit_completed_events().unwrap();
    let [event] = events.as_slice() else {
        panic!("expected one completion event, got {events:?}");
    };
    assert_eq!(event.work_unit_id, UNIT);
    assert_eq!(event.project_id, PROJECT);
    assert_eq!(event.parent_task_id, TASK);
    assert_eq!(event.previous_status, WorkUnitStatus::Todo);
    assert_eq!(event.evidence, [old, new]);

    let later = EvidenceRef {
        step_index: 3,
        ..new
    };
    store
        .complete_work_unit_with_evidence(fixture.complete(), &[later])
        .unwrap();
    assert_eq!(
        fixture.snapshot().work_unit(UNIT).unwrap().data.evidence,
        [old, new, later]
    );
    assert_eq!(
        fixture.store().work_unit_completed_events().unwrap(),
        events
    );
}

#[test]
fn rejected_evidence_completion_never_publishes_the_status_edit() {
    let fixture = Fixture::new();
    let source = fixture.set_unit(
        UNIT,
        "status: todo\nevidence: &shared []\ncustom: *shared\n",
    );
    let result = fixture.store().complete_work_unit_with_evidence(
        fixture.complete(),
        &[EvidenceRef {
            run_id: CAPTURED,
            step_index: 0,
        }],
    );
    assert!(matches!(result, Err(WriteError::InvalidDocument(_))));
    assert_eq!(fs::read_to_string(&fixture.unit).unwrap(), source);
    assert!(
        fixture
            .store()
            .work_unit_completed_events()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn competing_completions_use_the_revision_under_the_same_lock() {
    let fixture = Fixture::new();
    fixture.set_unit(UNIT, "status: todo\n");
    let barrier = Barrier::new(2);
    thread::scope(|scope| {
        let launch = || {
            let store = fixture.store();
            let request = fixture.complete();
            let barrier = &barrier;
            scope.spawn(move || {
                barrier.wait();
                store.complete_work_unit(request)
            })
        };
        let first = launch();
        let second = launch();
        let outcomes = [first.join().unwrap(), second.join().unwrap()];
        assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
        assert!(
            outcomes
                .iter()
                .any(|result| matches!(result, Err(WriteError::StaleRevision { id: UNIT })))
        );
    });
    assert_eq!(
        fixture.store().work_unit_completed_events().unwrap().len(),
        1
    );
}

#[test]
fn capture_creates_the_tasks_directory_when_missing() {
    let fixture = Fixture::new();
    let tasks = fixture.project.with_file_name("tasks");
    fs::remove_dir_all(&tasks).unwrap();
    let result = fixture.store().capture_task(fixture.capture()).unwrap();
    assert_eq!(result.path.parent(), Some(tasks.as_path()));
    assert!(result.path.is_file());
}

#[test]
fn capture_rejects_duplicate_ids_across_document_kinds() {
    let fixture = Fixture::new();
    fixture.set_unit(UNIT, "status: todo\n");
    let mut request = fixture.capture();
    request.task.id = UNIT;
    assert!(matches!(
        fixture.store().capture_task(request),
        Err(WriteError::DuplicateId { id: UNIT })
    ));
    assert_eq!(
        fixture.snapshot().source(UNIT),
        Some(fixture.unit.as_path())
    );
}

#[test]
fn capture_rejects_a_multiline_title_without_creating_a_file() {
    let fixture = Fixture::new();
    let mut request = fixture.capture();
    request.title = "Title\n## A second line".into();
    assert!(matches!(
        fixture.store().capture_task(request),
        Err(WriteError::InvalidDocument(_))
    ));
    assert!(fixture.snapshot().source(CAPTURED).is_none());
}

#[test]
fn complete_preserves_flow_mapping_and_status_alias_definition() {
    let fixture = Fixture::new();
    let source = format!(
        "---\n{{schema: mcbtodo.work-unit/v1, id: {UNIT}, project_id: {PROJECT}, parent_task_id: {TASK}, note: &state todo, status: *state}}\n---\n\n# Unit\n"
    );
    fs::write(&fixture.unit, &source).unwrap();
    fixture
        .store()
        .complete_work_unit(fixture.complete())
        .unwrap();
    assert_eq!(
        fs::read_to_string(&fixture.unit).unwrap(),
        source.replace("status: *state", "status: completed")
    );
}

#[test]
fn complete_preserves_block_scalar_indicator_comment() {
    let fixture = Fixture::new();
    let original = fixture.set_unit(UNIT, "status: |- # status comment\n  in_progress\n");
    fixture
        .store()
        .complete_work_unit(fixture.complete())
        .unwrap();
    assert_eq!(
        fs::read_to_string(&fixture.unit).unwrap(),
        original.replace("  in_progress\n", "  completed\n")
    );
}

#[test]
fn complete_rejects_shared_status_anchors_without_changing_extra_metadata() {
    let fixture = Fixture::new();
    let original = fixture.set_unit(UNIT, "status: &state todo\nextra: *state\n");
    assert!(matches!(
        fixture.store().complete_work_unit(fixture.complete()),
        Err(WriteError::InvalidDocument(_))
    ));
    assert_eq!(fs::read_to_string(&fixture.unit).unwrap(), original);
}

#[test]
fn complete_does_not_rewrite_an_already_completed_unit() {
    let fixture = Fixture::new();
    let original = fixture.set_unit(UNIT, "status: 'completed' # keep\n");
    let request = fixture.complete();
    let revision = request.expected_revision.clone();
    let result = fixture.store().complete_work_unit(request).unwrap();
    assert_eq!(result.revision, revision);
    assert_eq!(fs::read_to_string(&fixture.unit).unwrap(), original);
    assert!(
        fixture
            .store()
            .work_unit_completed_events()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn complete_does_not_reopen_a_cancelled_unit() {
    let fixture = Fixture::new();
    let original = fixture.set_unit(UNIT, "status: cancelled\n");
    assert!(matches!(
        fixture.store().complete_work_unit(fixture.complete()),
        Err(WriteError::Cancelled { id: UNIT })
    ));
    assert_eq!(fs::read_to_string(&fixture.unit).unwrap(), original);
    assert!(
        fixture
            .store()
            .work_unit_completed_events()
            .unwrap()
            .is_empty()
    );
}

#[cfg(unix)]
#[test]
fn capture_rejects_a_symlinked_tasks_directory() {
    let fixture = Fixture::new();
    let request = fixture.capture();
    let outside = tempfile::tempdir().unwrap();
    let tasks = fixture.project.with_file_name("tasks");
    fs::remove_dir_all(&tasks).unwrap();
    std::os::unix::fs::symlink(outside.path(), tasks).unwrap();
    assert!(matches!(
        fixture.store().capture_task(request),
        Err(WriteError::InvalidVault(_))
    ));
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn complete_rejects_a_symlinked_work_unit() {
    let fixture = Fixture::new();
    let original = fixture.set_unit(UNIT, "status: todo\n");
    let request = fixture.complete();
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("external.md");
    fs::write(&target, &original).unwrap();
    fs::remove_file(&fixture.unit).unwrap();
    std::os::unix::fs::symlink(&target, &fixture.unit).unwrap();
    assert!(matches!(
        fixture.store().complete_work_unit(request),
        Err(WriteError::InvalidVault(_))
    ));
    assert_eq!(fs::read_to_string(target).unwrap(), original);
}
