#![cfg(target_os = "linux")]

use std::{os::unix::net::UnixListener, thread, time::Duration};

use anyhow::Result;
use mcbtodo_client::daemon_client::{decide_workflow, workflow_run, workflow_runs, workflow_step};
use mcbtodo_domain::Id;
use mcbtodo_platform::WorkspacePaths;
use mcbtodo_protocol::{
    ErrorCode, Request, Response, wire,
    workflow::{WorkflowRequest, WorkflowResponse},
};
use mcbtodo_workflow::run::{RunState, RunSummary};

fn with_daemon(
    request: WorkflowRequest,
    response: Response,
    call: impl FnOnce(&WorkspacePaths) -> Result<()>,
) -> Result<()> {
    let home = tempfile::tempdir()?;
    let paths = WorkspacePaths::from_home(home.path().to_owned());
    paths.initialize()?;
    let listener = UnixListener::bind(paths.socket_path())?;
    let peer = thread::spawn(move || -> Result<()> {
        let (mut stream, _) = listener.accept()?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        let incoming: Request = wire::read(&mut stream)?;
        assert!(matches!(incoming, Request::Workflow(actual) if actual == request));
        wire::write(&mut stream, &response)?;
        Ok(())
    });
    call(&paths)?;
    peer.join().unwrap()
}

fn waiting_run(id: Id) -> RunSummary {
    RunSummary {
        id,
        revision: "revision-under-review".into(),
        due_at: None,
        schedule_id: None,
        source_run_id: None,
        source_event_id: None,
        state: RunState::Waiting,
        current_node: Some("review".into()),
        step_count: 1,
        detail: Some("Review the staged result".into()),
        resolution: None,
    }
}

#[test]
fn workflow_list_rejects_a_cursor_that_does_not_advance() -> Result<()> {
    let before = Some(Id::from_u128(1));
    with_daemon(
        WorkflowRequest::List { before },
        Response::Workflow(WorkflowResponse::Runs {
            runs: Vec::new(),
            next_before: before,
        }),
        |paths| {
            let Err(error) = workflow_runs(paths, before) else {
                panic!("repeated cursor accepted");
            };
            assert_eq!(error.to_string(), "daemon returned the same page cursor");
            Ok(())
        },
    )
}

#[test]
fn workflow_inspect_rejects_a_different_run() -> Result<()> {
    let run_id = Id::from_u128(1);
    with_daemon(
        WorkflowRequest::Inspect { run_id },
        Response::Workflow(WorkflowResponse::Run(waiting_run(Id::from_u128(2)))),
        |paths| {
            let error = workflow_run(paths, run_id).unwrap_err();
            assert_eq!(
                error.to_string(),
                format!("unexpected workflow response for run {run_id}")
            );
            Ok(())
        },
    )
}

#[test]
fn workflow_decision_passes_the_review_token_and_checks_the_receipt() -> Result<()> {
    let run_id = Id::from_u128(1);
    let review = waiting_run(run_id).approval_review();
    with_daemon(
        WorkflowRequest::Decide {
            run_id,
            node: "review".into(),
            approved: true,
            review: review.clone(),
        },
        Response::Workflow(WorkflowResponse::Run(waiting_run(Id::from_u128(2)))),
        |paths| {
            let error = decide_workflow(paths, run_id, "review".into(), true, review).unwrap_err();
            assert_eq!(
                error.to_string(),
                format!("unexpected workflow response for run {run_id}")
            );
            Ok(())
        },
    )
}

#[test]
fn workflow_step_preserves_a_daemon_error() -> Result<()> {
    let run_id = Id::from_u128(1);
    with_daemon(
        WorkflowRequest::Step { run_id, index: 0 },
        Response::Error {
            code: ErrorCode::InvalidRequest,
            message: "workflow step not found".into(),
        },
        |paths| {
            let error = workflow_step(paths, run_id, 0).unwrap_err();
            assert_eq!(error.to_string(), "InvalidRequest: workflow step not found");
            Ok(())
        },
    )
}
