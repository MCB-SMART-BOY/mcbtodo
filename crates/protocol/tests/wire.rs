use std::io::Cursor;

use mcbtodo_protocol::{ProtocolError, Request, wire};

#[test]
fn calendar_occurrences_round_trip_without_changing_the_json_shape() {
    use mcbtodo_protocol::Response;
    use serde_json::json;

    let body = json!({
        "result": "calendar",
        "items": [
            {"id": "00000000-0000-0000-0000-000000000001",
             "title": "跨月活动", "kind": "event", "busy": true,
             "occurrence": {"kind": "date_span", "start": "2026-11-30", "through": "2026-12-01"}},
            {"id": "00000000-0000-0000-0000-000000000002",
             "title": "Repeated hour", "kind": "time_block", "busy": true,
             "occurrence": {"kind": "date_time_span", "timezone": "America/New_York",
                            "start": "2026-11-01T01:50:00-04:00", "end": "2026-11-01T01:10:00-05:00"}},
            {"id": "00000000-0000-0000-0000-000000000003",
             "title": "截止日期", "kind": "deadline", "busy": false,
             "occurrence": {"kind": "date", "value": "2026-11-30"}},
            {"id": "00000000-0000-0000-0000-000000000004",
             "title": "Timed deadline", "kind": "deadline", "busy": false,
             "occurrence": {"kind": "date_time", "value": "2026-12-01T00:00:00+08:00",
                            "timezone": "Asia/Shanghai"}}
        ]
    });
    let decoded: Response = serde_json::from_value(body.clone()).unwrap();
    let mut frame = Vec::new();
    wire::write(&mut frame, &decoded).unwrap();
    let response: Response = wire::read(&mut Cursor::new(frame)).unwrap();
    assert_eq!(serde_json::to_value(response).unwrap(), body);
}

#[test]
fn notification_page_json_stays_compatible_with_existing_clients() -> serde_json::Result<()> {
    use mcbtodo_protocol::Response;
    use serde_json::json;

    let run = "00000000-0000-0000-0000-000000000001";
    let notification = "00000000-0000-0000-0000-000000000002";
    let reminder = "00000000-0000-0000-0000-000000000003";
    let now = "2026-11-22T00:00:00Z";
    for body in [
        json!({
            "result": "notifications",
            "notifications": [{
                "run_id": run, "revision": "published", "schedule_id": null,
                "state": "outcome_unknown", "finished_at": now, "detail": "interrupted",
            }],
            "next_before": run,
        }),
        json!({
            "result": "reminder_notifications",
            "notifications": [{
                "id": notification, "reminder_id": reminder, "target_id": null,
                "title": "提醒", "scheduled_at": now, "created_at": now,
            }],
            "next_before": null,
        }),
    ] {
        let decoded: Response = serde_json::from_value(body.clone())?;
        assert_eq!(serde_json::to_value(decoded)?, body);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
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
fn approval_review_survives_framing_and_legacy_decisions_have_no_review() {
    use mcbtodo_protocol::workflow::WorkflowRequest;
    use uuid::Uuid;

    let review = "a".repeat(64);
    let request = Request::Workflow(WorkflowRequest::Decide {
        run_id: Uuid::nil(),
        node: "review".into(),
        approved: true,
        review: Some(review.clone()),
    });
    let mut buffer = Vec::new();
    wire::write(&mut buffer, &request).unwrap();
    assert_eq!(
        wire::read::<Request>(&mut buffer.as_slice()).unwrap(),
        request
    );

    let mut body = serde_json::to_value(request).unwrap();
    assert_eq!(body["review"], review);
    body.as_object_mut().unwrap().remove("review");
    assert_eq!(
        serde_json::from_value::<Request>(body).unwrap(),
        Request::Workflow(WorkflowRequest::Decide {
            run_id: Uuid::nil(),
            node: "review".into(),
            approved: true,
            review: None,
        })
    );

    let rejection = Request::Workflow(WorkflowRequest::Decide {
        run_id: Uuid::nil(),
        node: "review".into(),
        approved: false,
        review: None,
    });
    let body = serde_json::to_value(&rejection).unwrap();
    assert!(!body.as_object().unwrap().contains_key("review"));
    assert_eq!(serde_json::from_value::<Request>(body).unwrap(), rejection);
}

#[test]
fn cron_schedule_preserves_the_stored_and_wire_json_shape() {
    use mcbtodo_protocol::workflow::{Fold, Missed, Trigger};

    let json = serde_json::json!({
        "kind": "cron",
        "expression": "30 1 * * *",
        "timezone": "America/New_York",
        "fold": "both"
    });
    let trigger = Trigger::Cron {
        expression: "30 1 * * *".into(),
        timezone: "America/New_York".into(),
        fold: Fold::Both,
    };
    assert_eq!(
        serde_json::from_value::<Trigger>(json.clone()).unwrap(),
        trigger
    );
    assert_eq!(serde_json::to_value(trigger).unwrap(), json);
    let json = serde_json::json!({"policy": "replay_limited", "limit": 16});
    let missed = Missed::ReplayLimited { limit: 16 };
    assert_eq!(
        serde_json::from_value::<Missed>(json.clone()).unwrap(),
        missed
    );
    assert_eq!(serde_json::to_value(missed).unwrap(), json);
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
fn schedule_response_preserves_the_persisted_summary_shape() {
    use mcbtodo_protocol::workflow::{ScheduleSummary, WorkflowResponse};

    let stored = serde_json::json!({
        "id": "00000000-0000-0000-0000-000000000004",
        "revision": "a".repeat(64),
        "trigger": {"kind": "dependency", "upstream_revision": "b".repeat(64)},
        "missed": {"policy": "skip"},
        "overlap": "queue",
        "next_at": null,
        "enabled": true,
        "missed_occurrences": 0,
        "overlap_skipped": 0
    });
    let summary: ScheduleSummary = serde_json::from_value(stored.clone()).unwrap();
    assert_eq!(summary.cascade_skipped, 0);
    assert_eq!(serde_json::to_value(&summary).unwrap(), stored);
    let mut expected_response = stored;
    expected_response["type"] = "schedule".into();
    assert_eq!(
        serde_json::to_value(WorkflowResponse::Schedule(summary)).unwrap(),
        expected_response
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
