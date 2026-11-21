use std::io::Cursor;

use mcbtodo_protocol::{ProtocolError, Request, wire};

#[cfg(unix)]
#[test]
fn client_sends_a_typed_request_and_returns_the_remote_error() {
    use mcbtodo_protocol::{ErrorCode, Response, request};
    use std::os::unix::net::UnixListener;
    use std::thread;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    let listener = UnixListener::bind(&path).unwrap();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        assert_eq!(wire::read::<Request>(&mut stream).unwrap(), Request::Pause);
        wire::write(
            &mut stream,
            &Response::Error {
                code: ErrorCode::Conflict,
                message: "conflict".to_owned(),
            },
        )
        .unwrap();
    });
    assert_eq!(
        request(path, Request::Pause).unwrap(),
        Response::Error {
            code: ErrorCode::Conflict,
            message: "conflict".to_owned(),
        }
    );
    handle.join().unwrap();
}

#[test]
fn rejects_an_oversized_frame_before_reading_its_body() {
    assert!(matches!(
        wire::read::<Request>(&mut Cursor::new(u32::MAX.to_be_bytes())),
        Err(ProtocolError::FrameSize(_))
    ));
}

#[test]
fn reports_version_before_decoding_an_unknown_request() {
    let json = br#"{"protocol_version":9,"body":{"command":"future_command"}}"#;
    let mut contents = u32::try_from(json.len()).unwrap().to_be_bytes().to_vec();
    contents.extend_from_slice(json);
    assert!(matches!(
        wire::read::<Request>(&mut Cursor::new(contents)),
        Err(ProtocolError::UnsupportedVersion(9))
    ));
}

#[test]
fn workflow_messages_round_trip_over_the_versioned_wire() {
    use mcbtodo_protocol::{
        Response,
        workflow::{RunState, RunSummary, WorkflowRequest, WorkflowResponse},
    };
    use uuid::Uuid;

    let run_id = Uuid::nil();
    let revision = "a".repeat(64);
    let request = Request::Workflow(WorkflowRequest::Enqueue {
        run_id,
        revision: revision.clone(),
        due_at: None,
    });
    let response = Response::Workflow(WorkflowResponse::Run(RunSummary {
        id: run_id,
        revision,
        due_at: None,
        schedule_id: None,
        source_run_id: None,
        source_event_id: None,
        state: RunState::Pending,
        current_node: None,
        step_count: 0,
        detail: None,
        resolution: None,
    }));
    let mut buffer = Vec::new();
    wire::write(&mut buffer, &request).unwrap();
    assert_eq!(
        wire::read::<Request>(&mut buffer.as_slice()).unwrap(),
        request
    );
    buffer.clear();
    wire::write(&mut buffer, &response).unwrap();
    assert_eq!(
        wire::read::<Response>(&mut buffer.as_slice()).unwrap(),
        response
    );
}

#[test]
fn completion_schedule_is_a_workflow_wire_request() {
    let trigger = serde_json::json!({
        "kind": "work_unit_completed",
        "work_unit_id": "00000000-0000-0000-0000-000000000003"
    });
    let request: mcbtodo_protocol::workflow::WorkflowRequest =
        serde_json::from_value(serde_json::json!({
            "operation": "add_schedule",
            "id": "00000000-0000-0000-0000-000000000004",
            "revision": "a".repeat(64),
            "trigger": trigger,
            "missed": {"policy": "skip"},
            "overlap": "queue"
        }))
        .expect("WorkUnit completion trigger should be accepted on the wire");
    let mut bytes = Vec::new();
    wire::write(&mut bytes, &Request::Workflow(request.clone())).unwrap();
    assert_eq!(
        wire::read::<Request>(&mut bytes.as_slice()).unwrap(),
        Request::Workflow(request)
    );
}

#[test]
fn uncertain_run_resolution_round_trips_over_the_versioned_wire() {
    use mcbtodo_protocol::{
        Request, Response,
        workflow::{ObservedOutcome, RunResolution, WorkflowRequest, WorkflowResponse},
    };
    use uuid::Uuid;

    let request = Request::Workflow(WorkflowRequest::ResolveUnknown {
        run_id: Uuid::nil(),
        outcome: ObservedOutcome::NotStarted,
        note: "Checked the external system.".into(),
    });
    let response = Response::Workflow(WorkflowResponse::Resolution(RunResolution {
        outcome: ObservedOutcome::NotStarted,
        note: "Checked the external system.".into(),
        recorded_at: chrono::DateTime::from_timestamp(1_800_000_000, 0).unwrap(),
    }));

    let mut bytes = Vec::new();
    wire::write(&mut bytes, &request).unwrap();
    assert_eq!(
        wire::read::<Request>(&mut bytes.as_slice()).unwrap(),
        request
    );
    bytes.clear();
    wire::write(&mut bytes, &response).unwrap();
    assert_eq!(
        wire::read::<Response>(&mut bytes.as_slice()).unwrap(),
        response
    );
}
