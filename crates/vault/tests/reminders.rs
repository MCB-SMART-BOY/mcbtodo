use std::fs;

use anyhow::Result;
use mcbtodo_domain::{Id, recurrence::OccurrenceWindow};
use mcbtodo_vault::Vault;

const RULE: &str = "\
schedule:
  kind: recurring
  rule:
    start: 2026-11-20
    time: '09:00:00'
    timezone: Asia/Shanghai
    frequency: daily
    interval: 1
    dst: { gap: skip, fold: earlier }
";

fn write(path: &std::path::Path, frontmatter: &str, schema: &str, id: u128) -> Result<()> {
    fs::create_dir_all(path.parent().unwrap())?;
    fs::write(
        path,
        format!(
            "---\nschema: mcbtodo.{schema}/v1\nid: {}\n{frontmatter}---\n\n# Example\n",
            Id::from_u128(id)
        ),
    )?;
    Ok(())
}

#[test]
fn validates_targets_and_suppresses_reminders_for_closed_work() -> Result<()> {
    let root = tempfile::tempdir()?;
    let root = root.path();
    write(&root.join("projects/a/project.md"), "", "project", 1)?;
    let task = root.join("projects/a/tasks/a.md");
    let project_ref = format!("project_id: {}\n", Id::from_u128(1));
    write(&task, &project_ref, "task", 2)?;
    let path = root.join("reminders/a.md");
    write(
        &path,
        &format!("target_id: {}\n{RULE}", Id::from_u128(2)),
        "reminder",
        3,
    )?;
    let vault = Vault::load(root, chrono_tz::UTC)?;
    let reminder = vault.reminders().next().unwrap();
    assert_eq!(reminder.path, path);
    let window = OccurrenceWindow::new(
        "2026-11-20T00:59:59Z".parse()?,
        "2026-11-20T01:00:00Z".parse()?,
    )?;
    assert_eq!(
        reminder.data.schedule.occurrences(window),
        ["2026-11-20T01:00:00Z".parse::<chrono::DateTime<chrono::Utc>>()?]
    );
    write(
        &task,
        &format!("{project_ref}status: completed\n"),
        "task",
        2,
    )?;
    assert_eq!(Vault::load(root, chrono_tz::UTC)?.reminders().count(), 0);
    fs::remove_file(task)?;
    assert!(
        Vault::load(root, chrono_tz::UTC)
            .unwrap_err()
            .to_string()
            .contains("missing Task or WorkUnit")
    );
    Ok(())
}

#[test]
fn rejects_invalid_rules_and_duplicate_vault_ids() -> Result<()> {
    let root = tempfile::tempdir()?;
    let path = root.path().join("reminders/a.md");
    for invalid in [
        RULE.replace("interval: 1", "interval: 0"),
        RULE.replace("frequency: daily", "frequency: secondly"),
        RULE.replace("fold: earlier", "fold: someday"),
        RULE.replace("interval: 1", "through: 2026-11-19"),
        RULE.replace("interval: 1", "inteval: 2"),
    ] {
        write(&path, &invalid, "reminder", 1)?;
        assert!(Vault::load(root.path(), chrono_tz::UTC).is_err());
    }
    write(&path, RULE, "reminder", 1)?;
    write(&root.path().join("projects/a/project.md"), "", "project", 1)?;
    assert!(
        Vault::load(root.path(), chrono_tz::UTC)
            .unwrap_err()
            .to_string()
            .contains("duplicate ID")
    );
    Ok(())
}
