use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/vault")
}

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_mcbtodo"))
}

fn query(vault: &Path, args: &[&str]) -> Output {
    cli()
        .args(["--timezone", "Asia/Shanghai", "--vault"])
        .arg(vault)
        .args(args)
        .output()
        .unwrap()
}

fn output(result: Output) -> String {
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).unwrap()
}

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[test]
fn same_documents_drive_calendar_outline_and_dependencies() {
    let vault = fixture();
    let outline = output(query(&vault, &["outline"]));
    assert!(outline.contains("  内存管理\n    阅读 Sv39 规范"));
    assert!(outline.contains("    实现地址转换 [blocked]"));

    let graph = output(query(&vault, &["graph"]));
    assert!(
        graph.contains("u30000000000040008000000000000001 --> u30000000000040008000000000000002")
    );

    let day = |date: &str| {
        serde_json::from_str::<Vec<serde_json::Value>>(&output(query(
            &vault,
            &["calendar", "--date", date, "--json"],
        )))
        .unwrap()
    };
    let day_with_plan = day("2026-11-20");
    assert!(
        day_with_plan
            .iter()
            .any(|entry| entry["kind"] == "time_block" && entry["busy"] == true)
    );
    let between = day("2026-11-21");
    assert!(
        between
            .iter()
            .any(|entry| { entry["kind"] == "active_period" && entry["busy"] == false })
    );
    assert!(between.iter().any(|entry| {
        entry["kind"] == "task_occurrence" && entry["title"] == "Weekly review (2026-11-21)"
    }));
    let event_day = day("2026-11-25");
    assert!(
        event_day
            .iter()
            .any(|entry| entry["kind"] == "deadline" && entry["busy"] == false)
    );
    assert!(
        event_day
            .iter()
            .any(|entry| entry["title"] == "校园活动" && entry["busy"] == true)
    );
    assert!(
        event_day
            .iter()
            .any(|entry| entry["title"] == "远程讨论" && entry["busy"] == false)
    );
}

#[test]
fn calendar_rejects_dates_outside_the_shared_domain_format() {
    for date in ["0000-01-01", "2023-02-29", "2024-2-09"] {
        let result = cli().args(["calendar", "--date", date]).output().unwrap();
        assert!(!result.status.success(), "CLI unexpectedly accepted {date}");
    }
}

#[test]
fn timer_start_rejects_unknown_work_unit_before_contacting_the_daemon() {
    let home = tempfile::tempdir().unwrap();
    let initialized = cli().env("HOME", home.path()).arg("init").output().unwrap();
    assert!(
        initialized.status.success(),
        "{}",
        String::from_utf8_lossy(&initialized.stderr)
    );
    copy_tree(&fixture(), &home.path().join(".mcbtodo/vault"));

    let result = cli()
        .env("HOME", home.path())
        .args([
            "timer",
            "start",
            "--unit",
            "30000000-0000-4000-8000-000000000099",
        ])
        .output()
        .unwrap();

    assert!(!result.status.success());
    let error = String::from_utf8_lossy(&result.stderr);
    assert!(error.contains("no work unit with ID"), "{error}");
    assert!(!error.contains("contacting mcbtodo-daemon"), "{error}");
}

#[test]
fn reminder_creation_rejects_an_external_vault_before_writing() {
    let home = tempfile::tempdir().unwrap();
    let external = tempfile::tempdir().unwrap();
    let result = cli()
        .env("HOME", home.path())
        .args(["--timezone", "Asia/Shanghai", "--vault"])
        .arg(external.path())
        .args([
            "vault",
            "create-reminder",
            "--title",
            "Review",
            "--schedule",
            r#"{"kind":"once","at":{"value":"2026-11-21T09:00:00+08:00","timezone":"Asia/Shanghai"}}"#,
        ])
        .output()
        .unwrap();

    assert!(!result.status.success());
    let error = String::from_utf8_lossy(&result.stderr);
    assert!(
        error.contains("background reminders are only supported"),
        "{error}"
    );
    assert_eq!(fs::read_dir(external.path()).unwrap().count(), 0);
}

#[test]
fn inbox_capture_and_completion_export_do_not_load_unrelated_vault_documents() {
    let temp = tempfile::tempdir().unwrap();
    let vault = temp.path().join("vault");
    fs::create_dir_all(&vault).unwrap();
    fs::write(
        vault.join("unrelated.md"),
        "not valid Markdown front matter",
    )
    .unwrap();

    let captured = cli()
        .args(["--timezone", "Asia/Shanghai", "--vault"])
        .arg(&vault)
        .args(["vault", "capture-inbox", "--title", "Quick note"])
        .output()
        .unwrap();
    assert!(
        captured.status.success(),
        "{}",
        String::from_utf8_lossy(&captured.stderr)
    );
    assert!(vault.join("inbox").is_dir());

    let events = cli()
        .args(["--timezone", "Asia/Shanghai", "--vault"])
        .arg(&vault)
        .args(["vault", "completion-events"])
        .output()
        .unwrap();
    assert!(
        events.status.success(),
        "{}",
        String::from_utf8_lossy(&events.stderr)
    );
    assert!(events.stdout.is_empty());
}

#[test]
fn calendar_ical_export_preserves_date_bounds_instants_and_deadline_semantics() {
    let ical = output(query(
        &fixture(),
        &[
            "calendar",
            "export-ical",
            "--from",
            "2026-11-25",
            "--through",
            "2026-11-25",
        ],
    ));

    assert!(ical.starts_with("BEGIN:VCALENDAR\r\n"));
    assert!(ical.contains("SUMMARY:校园活动\r\n"));
    assert!(ical.contains("DTSTART;VALUE=DATE:20261125\r\n"));
    assert!(ical.contains("DTEND;VALUE=DATE:20261126\r\n"));
    assert!(ical.contains("SUMMARY:远程讨论\r\n"));
    assert!(ical.contains("DTSTART:20261125T050000Z\r\n"));
    assert!(ical.contains("DTEND:20261125T070000Z\r\n"));
    assert!(ical.contains("BEGIN:VTODO\r\n"));
    assert!(ical.contains("DUE;VALUE=DATE:20261125\r\n"));
}

#[test]
fn calendar_ical_import_publishes_supported_events_and_preserves_time_semantics() {
    let temp = tempfile::tempdir().unwrap();
    copy_tree(&fixture(), temp.path());
    let input = temp.path().join("import.ics");
    fs::write(
        &input,
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n\
         BEGIN:VEVENT\r\nUID:date@example.test\r\nSUMMARY:Imported date\r\n\
         DTSTART;VALUE=DATE:20261201\r\nDTEND;VALUE=DATE:20261203\r\n\
         DESCRIPTION:First line\\nSecond line\r\nEND:VEVENT\r\n\
         BEGIN:VEVENT\r\nUID:time@example.test\r\nSUMMARY:Imported time\r\n\
         DTSTART;TZID=Asia/Tokyo:20261204T100000\r\n\
         DTEND;TZID=Asia/Tokyo:20261204T110000\r\nEND:VEVENT\r\n\
         END:VCALENDAR\r\n",
    )
    .unwrap();

    let imported = query(
        temp.path(),
        &["calendar", "import-ical", input.to_str().unwrap()],
    );
    assert_eq!(output(imported), "Imported 2 calendar events\n");
    output(query(temp.path(), &["validate"]));

    let date_id = mcbtodo_domain::Id::new_v5(
        &mcbtodo_domain::Id::NAMESPACE_URL,
        b"mcbtodo:ical:date@example.test",
    );
    let time_id = mcbtodo_domain::Id::new_v5(
        &mcbtodo_domain::Id::NAMESPACE_URL,
        b"mcbtodo:ical:time@example.test",
    );
    let date_path = temp.path().join(format!("calendar/{date_id}.md"));
    let date_document = fs::read_to_string(date_path).unwrap();
    assert!(
        date_document.contains("2026-12-01") && date_document.contains("2026-12-02"),
        "{date_document}"
    );
    assert!(date_document.contains("First line\nSecond line"));

    let time_path = temp.path().join(format!("calendar/{time_id}.md"));
    let time_document = fs::read_to_string(time_path).unwrap();
    assert!(time_document.contains("start: \"2026-12-04T10:00:00+09:00\""));
    assert!(time_document.contains("end: \"2026-12-04T11:00:00+09:00\""));
}

#[test]
fn calendar_ical_import_conflict_does_not_publish_any_event_in_the_batch() {
    let temp = tempfile::tempdir().unwrap();
    copy_tree(&fixture(), temp.path());
    let collision_id = mcbtodo_domain::Id::new_v5(
        &mcbtodo_domain::Id::NAMESPACE_URL,
        b"mcbtodo:ical:existing@example.test",
    );
    let existing_path = temp.path().join(format!("calendar/{collision_id}.md"));
    fs::write(
        &existing_path,
        format!(
            "---\nschema: mcbtodo.calendar-event/v1\nid: {collision_id}\nwhen:\n  kind: date_span\n  start: '2026-12-01'\n  through: '2026-12-01'\nrecurrence: null\navailability: busy\ntags: []\n---\n\n# Existing event\n"
        ),
    )
    .unwrap();
    let existing_bytes = fs::read(&existing_path).unwrap();
    let input = temp.path().join("conflict.ics");
    fs::write(
        &input,
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n\
         BEGIN:VEVENT\r\nUID:new@example.test\r\nSUMMARY:Would be new\r\n\
         DTSTART;VALUE=DATE:20261202\r\nEND:VEVENT\r\n\
         BEGIN:VEVENT\r\nUID:existing@example.test\r\nSUMMARY:Collision\r\n\
         DTSTART;VALUE=DATE:20261203\r\nEND:VEVENT\r\n\
         END:VCALENDAR\r\n",
    )
    .unwrap();

    let result = query(
        temp.path(),
        &["calendar", "import-ical", input.to_str().unwrap()],
    );
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("already exists"));
    assert_eq!(fs::read(&existing_path).unwrap(), existing_bytes);
    let new_id = mcbtodo_domain::Id::new_v5(
        &mcbtodo_domain::Id::NAMESPACE_URL,
        b"mcbtodo:ical:new@example.test",
    );
    assert!(!temp.path().join(format!("calendar/{new_id}.md")).exists());
}

#[test]
fn renamed_file_keeps_links_and_invalid_graph_leaves_documents_untouched() {
    let temp = tempfile::tempdir().unwrap();
    copy_tree(&fixture(), temp.path());
    let units = temp.path().join("projects/rust-os/units");
    let renamed = units.join("spec-renamed.md");
    fs::rename(units.join("read-spec.md"), &renamed).unwrap();
    output(query(temp.path(), &["validate"]));

    let source = fs::read_to_string(&renamed).unwrap().replace(
        "status: completed",
        "status: completed\ndepends_on: [30000000-0000-4000-8000-000000000002]",
    );
    fs::write(&renamed, &source).unwrap();
    let failed = query(temp.path(), &["validate"]);
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("cycle"));
    assert_eq!(fs::read_to_string(renamed).unwrap(), source);
}

#[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
#[test]
fn vault_commands_create_and_schedule_work_with_dependency_checks() {
    let temp = tempfile::tempdir().unwrap();
    copy_tree(&fixture(), temp.path());
    let vault = temp.path();
    let project = "10000000-0000-4000-8000-000000000099";
    let task = "20000000-0000-4000-8000-000000000099";
    let unit = "30000000-0000-4000-8000-000000000099";
    let mapping = "30000000-0000-4000-8000-000000000002";
    let command = |args: &[&str]| {
        let args: Vec<_> = std::iter::once("vault")
            .chain(args.iter().copied())
            .collect();
        query(vault, &args)
    };
    output(command(&[
        "create-project",
        "--id",
        project,
        "--slug",
        "cli-test",
        "--title",
        "CLI project",
        "--active-from",
        "2026-11-20",
        "--active-through",
        "2027-03-01",
        "--deadline",
        "2027-03-01",
    ]));
    output(command(&[
        "capture",
        "--project",
        project,
        "--id",
        task,
        "--title",
        "CLI task",
    ]));
    output(command(&[
        "create-work-unit",
        "--project",
        project,
        "--task",
        task,
        "--id",
        unit,
        "--title",
        "CLI unit",
        "--depends-on",
        mapping,
    ]));

    let blocked = command(&["complete", "--unit", unit]);
    assert!(!blocked.status.success());
    assert!(String::from_utf8_lossy(&blocked.stderr).contains("incomplete prerequisites"));
    let unit_file = vault.join(format!("projects/cli-test/units/{unit}.md"));
    let before = fs::read(&unit_file).unwrap();
    let invalid_time = command(&[
        "create-time-block",
        "--unit",
        unit,
        "--start",
        "2026-11-20T12:00:00Z",
        "--end",
        "2026-11-20T14:00:00Z",
    ]);
    assert!(!invalid_time.status.success());
    assert!(String::from_utf8_lossy(&invalid_time.stderr).contains("offset"));
    assert_eq!(fs::read(unit_file).unwrap(), before);

    output(command(&[
        "create-time-block",
        "--unit",
        unit,
        "--start",
        "2026-11-20T12:00:00+08:00",
        "--end",
        "2026-11-20T14:00:00+08:00",
        "--availability",
        "free",
    ]));
    output(command(&["complete", "--unit", mapping]));
    output(command(&["complete", "--unit", unit]));
    let calendar = output(query(vault, &["calendar", "--date", "2026-11-20"]));
    assert!(calendar.contains("block · free · CLI unit · 11-20 12:00 +08:00 → 11-20 14:00 +08:00"));
    output(query(vault, &["validate"]));
}

#[test]
fn vault_commands_set_and_clear_a_deadline() {
    let temp = tempfile::tempdir().unwrap();
    copy_tree(&fixture(), temp.path());
    let vault = temp.path();
    let unit = "30000000-0000-4000-8000-000000000001";
    let command = |args: &[&str]| {
        let args: Vec<_> = std::iter::once("vault")
            .chain(args.iter().copied())
            .collect();
        query(vault, &args)
    };

    output(command(&[
        "set-deadline",
        "--id",
        unit,
        "--date",
        "2026-12-10",
    ]));
    let date = serde_json::from_str::<Vec<serde_json::Value>>(&output(query(
        vault,
        &["calendar", "--date", "2026-12-10", "--json"],
    )))
    .unwrap();
    assert!(date.iter().any(|entry| {
        entry["kind"] == "deadline" && entry["id"] == unit && entry["busy"] == false
    }));

    output(command(&["clear-deadline", "--id", unit]));
    let date = serde_json::from_str::<Vec<serde_json::Value>>(&output(query(
        vault,
        &["calendar", "--date", "2026-12-10", "--json"],
    )))
    .unwrap();
    assert!(
        !date
            .iter()
            .any(|entry| entry["kind"] == "deadline" && entry["id"] == unit)
    );
    output(query(vault, &["validate"]));
}

#[test]
fn vault_command_creates_a_targeted_recurring_reminder() {
    let home = tempfile::tempdir().unwrap();
    let initialized = cli().env("HOME", home.path()).arg("init").output().unwrap();
    assert!(
        initialized.status.success(),
        "{}",
        String::from_utf8_lossy(&initialized.stderr)
    );
    let vault = home.path().join(".mcbtodo/vault");
    copy_tree(&fixture(), &vault);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(home.path().join(".mcbtodo"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
    let run = |args: &[&str]| {
        cli()
            .env("HOME", home.path())
            .args(["--timezone", "Asia/Shanghai", "--vault"])
            .arg(&vault)
            .args(args)
            .output()
            .unwrap()
    };
    let schedule = r#"{"kind":"recurring","rule":{"start":"2026-11-21","time":"09:00:00","timezone":"Asia/Shanghai","frequency":"weekly","interval":1,"dst":{"gap":"skip","fold":"earlier"}}}"#;
    output(run(&[
        "vault",
        "create-reminder",
        "--title",
        "Weekly review",
        "--target",
        "20000000-0000-4000-8000-000000000001",
        "--schedule",
        schedule,
    ]));
    let reminders = vault.join("reminders");
    let path = fs::read_dir(reminders)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let source = fs::read_to_string(path).unwrap();
    assert!(source.contains("schema: mcbtodo.reminder/v1"));
    assert!(source.contains("Weekly review"));
    assert!(source.contains("frequency: weekly"));
    output(run(&["validate"]));
}

#[test]
fn inbox_capture_list_and_triage_are_a_complete_cli_workflow() {
    let temp = tempfile::tempdir().unwrap();
    copy_tree(&fixture(), temp.path());
    let id = "90000000-0000-4000-8000-000000000001";
    output(query(
        temp.path(),
        &[
            "vault",
            "capture-inbox",
            "--id",
            id,
            "--title",
            "Unsorted thought",
            "--body",
            "Keep these notes",
            "--tag",
            "idea",
        ],
    ));
    let listed = output(query(temp.path(), &["vault", "list-inbox"]));
    assert!(listed.contains("Unsorted thought"));
    assert!(listed.contains(id));
    let entry: serde_json::Value = serde_json::from_str(listed.lines().next().unwrap()).unwrap();
    assert_eq!(entry["id"], id);
    assert_eq!(entry["title"], "Unsorted thought");
    assert_eq!(entry["tags"], serde_json::json!(["idea"]));
    assert!(
        entry["revision"]
            .as_str()
            .is_some_and(|revision| revision.len() == 64)
    );
    output(query(
        temp.path(),
        &[
            "vault",
            "triage-inbox",
            "--id",
            id,
            "--project",
            "10000000-0000-4000-8000-000000000001",
        ],
    ));
    let path = temp.path().join(format!("inbox/{id}.md"));
    let source = fs::read_to_string(path).unwrap();
    assert!(source.contains("project_id: 10000000-0000-4000-8000-000000000001"));
    assert!(source.ends_with("Keep these notes"));
    let project_tasks = temp.path().join("projects/rust-os/tasks");
    let task_sources = fs::read_dir(project_tasks)
        .unwrap()
        .map(|entry| fs::read_to_string(entry.unwrap().path()).unwrap())
        .collect::<Vec<_>>();
    let converted = task_sources
        .iter()
        .filter(|source| source.contains("# Unsorted thought"))
        .collect::<Vec<_>>();
    assert_eq!(converted.len(), 1);
    assert!(converted[0].ends_with("Keep these notes"));
    output(query(temp.path(), &["validate"]));
}

#[test]
fn init_uses_the_requested_home_layout_and_preserves_config() {
    let temp = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| cli().env("HOME", temp.path()).args(args).output().unwrap();
    output(run(&["init"]));
    let config = temp.path().join(".config/mcbtodo/config.toml");
    assert!(temp.path().join(".mcbtodo/vault").is_dir());
    assert!(temp.path().join(".mcbtodo/state").is_dir());
    fs::write(&config, "timezone = \"Asia/Shanghai\"\n").unwrap();
    output(run(&["init"]));
    assert_eq!(
        fs::read_to_string(config).unwrap(),
        "timezone = \"Asia/Shanghai\"\n"
    );
    assert!(output(run(&["validate"])).contains("Asia/Shanghai"));
}
