use std::fs;

use mcbtodo_ports::workflow::WorkflowRunCatalog;
use mcbtodo_protocol::workflow::{Missed, Overlap};
use rusqlite::Connection;

use super::*;

fn dependency(id: Uuid, upstream: &str, downstream: &str) -> Result<WorkflowRequest> {
    Ok(serde_json::from_value(json!({
        "operation": "add_schedule",
        "id": id,
        "revision": downstream,
        "trigger": { "kind": "dependency", "upstream_revision": upstream },
        "missed": Missed::Skip,
        "overlap": Overlap::Queue,
    }))?)
}

fn add(runtime: &mut Runtime, upstream: &str, downstream: &str) -> Result<Uuid> {
    let id = Uuid::new_v4();
    let response = runtime.handle(dependency(id, upstream, downstream)?);
    let Response::Workflow(WorkflowResponse::Schedule(schedule)) = response else {
        anyhow::bail!("expected dependency schedule, got {response:?}");
    };
    assert_eq!(schedule.id, id);
    assert!(schedule.enabled);
    assert_eq!(schedule.next_at, None);
    Ok(id)
}

fn inspect(runtime: &mut Runtime, id: Uuid) -> Result<ScheduleSummary> {
    match runtime.handle(WorkflowRequest::InspectSchedule { id }) {
        Response::Workflow(WorkflowResponse::Schedule(schedule)) => Ok(schedule),
        other => anyhow::bail!("expected schedule, got {other:?}"),
    }
}

fn pending(store: &dyn WorkflowRunCatalog, revision: &str) -> Result<RunSummary> {
    let id = Uuid::new_v4();
    store.enqueue_if_absent(id, revision, None)?;
    Ok(store.get(id)?.unwrap())
}

fn publish(dir: &Path, name: &str) -> Result<String> {
    let path = dir.join("dependency.yaml");
    fs::write(
        &path,
        format!(
            "schema: mcbtodo.workflow/v1\nid: {name}\nname: {name}\nentry: report\n\
             nodes:\n  report: {{kind: action, action: report.create}}\n\
             literals:\n  - {{to: {{node: report, port: summary}}, value: {name}}}\n"
        ),
    )?;
    Ok(RevisionStore::new(dir).publish(path)?.hash().into())
}

#[test]
fn successful_pinned_revision_queues_downstream_across_restart() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let upstream = publish(dir.path(), "upstream")?;
    let downstream = publish(dir.path(), "downstream")?;
    let mut runtime = Runtime::open_for_test(dir.path())?;
    let schedule_id = add(&mut runtime, &upstream, &downstream)?;
    let source = Uuid::new_v4();
    assert!(matches!(
        runtime.handle(WorkflowRequest::Enqueue {
            run_id: source,
            revision: upstream.clone(),
            due_at: None,
        }),
        Response::Workflow(WorkflowResponse::Run(_))
    ));
    let original = runtime.run(source)?;
    runtime.tick()?;
    runtime.tick()?;
    assert_eq!(runtime.run(source)?.state, RunState::Succeeded);
    drop(runtime);

    let mut runtime = Runtime::open_for_test(dir.path())?;
    let (runs, _) = runtime.store.list(None)?;
    assert_eq!(runs.len(), 2);
    let child = &runs[0];
    assert_eq!(child.revision, downstream);
    assert_eq!(child.schedule_id, Some(schedule_id));
    assert_eq!(child.source_run_id, Some(source));
    assert_eq!(child.state, RunState::Pending);
    assert!(
        runtime
            .store
            .transition(&original, RunState::Succeeded, None, None)
            .is_err()
    );
    runtime
        .store
        .transition(&runtime.run(source)?, RunState::Succeeded, None, None)?;
    runtime.tick()?;
    runtime.tick()?;
    assert_eq!(runtime.run(child.id)?.state, RunState::Succeeded);
    assert_eq!(runtime.store.list(None)?.0.len(), 2);

    // A different source run of the same revision is a new occurrence.
    let next = pending(runtime.store.as_ref(), &upstream)?;
    runtime
        .store
        .transition(&next, RunState::Succeeded, None, None)?;
    assert_eq!(runtime.store.list(None)?.0[0].source_run_id, Some(next.id));
    assert_eq!(runtime.store.list(None)?.0.len(), 4);
    Ok(())
}

#[test]
fn dependency_ignores_history_other_revisions_and_nonsuccess() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let upstream = publish(dir.path(), "upstream")?;
    let other = publish(dir.path(), "other")?;
    let downstream = publish(dir.path(), "downstream")?;
    let mut runtime = Runtime::open_for_test(dir.path())?;
    let old = pending(runtime.store.as_ref(), &upstream)?;
    runtime
        .store
        .transition(&old, RunState::Succeeded, None, None)?;
    let schedule = add(&mut runtime, &upstream, &downstream)?;
    assert_eq!(runtime.store.next_wake_at(false)?, None);
    for (revision, state) in [
        (&upstream, RunState::Failed),
        (&upstream, RunState::Cancelled),
        (&upstream, RunState::OutcomeUnknown),
        (&other, RunState::Succeeded),
    ] {
        let run = pending(runtime.store.as_ref(), revision)?;
        runtime.store.transition(&run, state, None, None)?;
    }
    assert!(matches!(
        runtime.handle(WorkflowRequest::DisableSchedule { id: schedule }),
        Response::Workflow(WorkflowResponse::Schedule(summary)) if !summary.enabled
    ));
    let future = pending(runtime.store.as_ref(), &upstream)?;
    runtime
        .store
        .transition(&future, RunState::Succeeded, None, None)?;
    let (runs, _) = runtime.store.list(None)?;
    assert_eq!(runs.len(), 6);
    assert!(runs.iter().all(|run| run.revision != downstream));
    Ok(())
}

#[test]
fn dependency_rejects_unpublished_revisions_cycles_and_duplicate_edges() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let a = publish(dir.path(), "alpha")?;
    let b = publish(dir.path(), "beta")?;
    let c = publish(dir.path(), "charlie")?;
    let unpublished = "0".repeat(64);
    let mut runtime = Runtime::open_for_test(dir.path())?;
    let first = add(&mut runtime, &a, &b)?;
    assert_eq!(
        runtime.handle(dependency(first, &a, &b)?),
        Response::Workflow(WorkflowResponse::Schedule(inspect(&mut runtime, first)?)),
    );
    add(&mut runtime, &b, &c)?;
    let rejected_id = Uuid::new_v4();
    for (upstream, downstream) in [
        (&a, &a),
        (&c, &a),
        (&a, &b),
        (&unpublished, &a),
        (&a, &unpublished),
    ] {
        assert!(matches!(
            runtime.handle(dependency(rejected_id, upstream, downstream)?),
            Response::Error {
                code: ErrorCode::InvalidRequest,
                ..
            }
        ));
        assert!(runtime.store.schedule(rejected_id)?.is_none());
    }
    Ok(())
}

#[test]
fn replacing_an_edge_cannot_reenter_an_inflight_lineage() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let a = publish(dir.path(), "alpha")?;
    let b = publish(dir.path(), "beta")?;
    let mut runtime = Runtime::open_for_test(dir.path())?;
    let first = add(&mut runtime, &a, &b)?;
    let root = pending(runtime.store.as_ref(), &a)?;
    runtime
        .store
        .transition(&root, RunState::Succeeded, None, None)?;
    let child = runtime.store.next_pending_at(Utc::now())?.unwrap();
    assert!(matches!(
        runtime.handle(WorkflowRequest::DisableSchedule { id: first }),
        Response::Workflow(WorkflowResponse::Schedule(summary)) if !summary.enabled
    ));
    let reversed = add(&mut runtime, &b, &a)?;
    drop(runtime);

    let mut runtime = Runtime::open_for_test(dir.path())?;
    runtime
        .store
        .transition(&child, RunState::Succeeded, None, None)?;
    assert_eq!(runtime.store.list(None)?.0.len(), 2);
    assert_eq!(inspect(&mut runtime, reversed)?.cascade_skipped, 1);

    // The reverse edge remains usable from a fresh root.
    let fresh = pending(runtime.store.as_ref(), &b)?;
    runtime
        .store
        .transition(&fresh, RunState::Succeeded, None, None)?;
    assert_eq!(
        runtime
            .store
            .next_pending_at(Utc::now())?
            .unwrap()
            .source_run_id,
        Some(fresh.id)
    );
    assert_eq!(inspect(&mut runtime, reversed)?.cascade_skipped, 1);
    Ok(())
}

#[test]
fn dependency_chain_stops_at_eight_edges_and_persists_its_skip() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let mut runtime = Runtime::open_for_test(dir.path())?;
    let revisions = (0..10)
        .map(|i| publish(dir.path(), &format!("revision-{i}")))
        .collect::<Result<Vec<_>>>()?;
    let mut last = Uuid::nil();
    for edge in revisions.windows(2) {
        last = add(&mut runtime, &edge[0], &edge[1])?;
    }
    let root = pending(runtime.store.as_ref(), &revisions[0])?;
    let mut run = root;
    for (depth, revision) in revisions[..9].iter().enumerate() {
        if depth == 8 {
            drop(runtime);
            runtime = Runtime::open_for_test(dir.path())?;
        }
        assert_eq!(&run.revision, revision);
        runtime
            .store
            .transition(&run, RunState::Succeeded, None, None)?;
        if depth < 8 {
            let next = runtime.store.next_pending_at(Utc::now())?.unwrap();
            assert_eq!(next.source_run_id, Some(run.id));
            run = next;
        }
    }
    assert!(runtime.store.next_pending_at(Utc::now())?.is_none());
    assert_eq!(runtime.store.list(None)?.0.len(), 9);
    assert_eq!(inspect(&mut runtime, last)?.cascade_skipped, 1);
    Ok(())
}

#[test]
fn dependency_fanout_and_total_descendants_are_bounded() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let mut runtime = Runtime::open_for_test(dir.path())?;
    let root = publish(dir.path(), "root")?;
    let leaves = (0..8)
        .map(|i| publish(dir.path(), &format!("leaf-{i}")))
        .collect::<Result<Vec<_>>>()?;
    let mut leaf_rules = Vec::new();
    for i in 0..16 {
        let branch = publish(dir.path(), &format!("branch-{i}"))?;
        add(&mut runtime, &root, &branch)?;
        for leaf in &leaves {
            leaf_rules.push(add(&mut runtime, &branch, leaf)?);
        }
    }
    assert!(matches!(
        runtime.handle(dependency(Uuid::new_v4(), &root, &leaves[0])?),
        Response::Error {
            code: ErrorCode::InvalidRequest,
            ..
        }
    ));
    let mut count = 0;
    pending(runtime.store.as_ref(), &root)?;
    while let Some(run) = runtime.store.next_pending_at(Utc::now())? {
        count += 1;
        runtime
            .store
            .transition(&run, RunState::Succeeded, None, None)?;
    }
    assert_eq!(count, 129); // the root plus 128 descendants
    let skipped: u64 = leaf_rules
        .into_iter()
        .map(|id| Ok(inspect(&mut runtime, id)?.cascade_skipped))
        .sum::<Result<u64>>()?;
    assert_eq!(skipped, 16);
    Ok(())
}

#[test]
fn failed_fanout_rolls_back_source_success_children_and_notification() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let upstream = publish(dir.path(), "upstream")?;
    let a = publish(dir.path(), "alpha")?;
    let b = publish(dir.path(), "beta")?;
    let mut runtime = Runtime::open_for_test(dir.path())?;
    add(&mut runtime, &upstream, &a)?;
    add(&mut runtime, &upstream, &b)?;
    let source = pending(runtime.store.as_ref(), &upstream)?;
    let db = Connection::open(dir.path().join("workflow.sqlite3"))?;
    db.execute_batch(
        "CREATE TRIGGER fail_second_child BEFORE INSERT ON workflow_runs
         WHEN NEW.schedule_id IS NOT NULL AND EXISTS (
             SELECT 1 FROM workflow_runs WHERE schedule_id IS NOT NULL
         )
         BEGIN SELECT RAISE(ABORT, 'test failure on second child'); END;",
    )?;
    let error = runtime
        .store
        .transition(&source, RunState::Succeeded, None, None)
        .unwrap_err();
    assert!(error.to_string().contains("test failure on second child"));
    assert_eq!(
        runtime.store.get(source.id)?.unwrap().state,
        RunState::Pending
    );
    assert_eq!(runtime.store.list(None)?.0.len(), 1);
    assert!(
        runtime
            .notifications()
            .workflow_page(None)?
            .notifications
            .is_empty()
    );
    db.execute_batch("DROP TRIGGER fail_second_child;")?;
    runtime
        .store
        .transition(&source, RunState::Succeeded, None, None)?;
    assert_eq!(runtime.store.list(None)?.0.len(), 3);
    assert_eq!(
        runtime.notifications().workflow_page(None)?.notifications[0].run_id,
        source.id
    );
    Ok(())
}
