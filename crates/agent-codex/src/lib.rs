//! Bounded, single-turn client for the Codex app-server stdio protocol.

use std::{
    collections::VecDeque,
    io::{self, BufRead, Cursor, Read, Write},
    sync::atomic::AtomicBool,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use mcbtodo_ports::{AgentError, AgentExecutionContext, AgentResult};
use serde_json::{Value, json};

#[cfg(target_os = "linux")]
mod provider;
#[cfg(target_os = "linux")]
pub use provider::CodexProvider;

const MAX_REQUEST: usize = 4096;
const MAX_FRAME: usize = 64 * 1024;
const MAX_SESSION_RESPONSE: u64 = 1024 * 1024;
const MAX_CONTROL_MESSAGES: usize = 32;
const MAX_STREAM_MESSAGES: usize = 4096;
const MAX_OUTPUT: usize = 16 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const INTERRUPT_GRACE: Duration = Duration::from_millis(150);

/// How the client drives a single isolated turn.
pub enum TurnAction<'a> {
    /// Interrupt immediately after app-server acknowledges the turn.
    Interrupt,
    /// Collect a structured result as a JSON object.
    Structured(&'a Value),
}

#[derive(Debug, PartialEq)]
pub enum TurnResult {
    Interrupted,
    Structured(AgentResult),
}

/// Drive one fresh app-server stdio session in the Linux scratch layout.
///
/// The caller must supply nonblocking streams and own process isolation,
/// lifecycle, and executable selection. The server
/// must see only the intended `/scratch` workspace and `/scratch/.codex` home.
/// The caller terminates and reaps the server on timeout or cancellation.
/// Once `turn/start` may have been sent, an error is `OutcomeUnknown`.
/// A structured result remains untrusted until the caller validates it against
/// the expected application schema and performs its own authorization checks.
pub fn run_turn(
    input: &mut impl BufRead,
    output: &mut impl Write,
    context: AgentExecutionContext<'_>,
    prompt: &str,
    action: TurnAction<'_>,
) -> Result<TurnResult, AgentError> {
    let mut dispatched = false;
    // Include startup notifications, early deltas and interruption replies in
    // the same byte budget. The per-frame and assistant text limits still apply.
    let mut input = input.take(MAX_SESSION_RESPONSE);
    let result = drive_turn(&mut input, output, context, prompt, action, &mut dispatched).and_then(
        |result| {
            check_context(context)?;
            Ok(result)
        },
    );
    result.map_err(|error| {
        if dispatched {
            AgentError::OutcomeUnknown(format!("{error:#}"))
        } else {
            error
                .downcast::<AgentError>()
                .unwrap_or_else(|error| AgentError::Transport(format!("{error:#}")))
        }
    })
}

fn check_context(context: AgentExecutionContext<'_>) -> Result<(), AgentError> {
    if context.cancellation.is_cancelled() {
        return Err(AgentError::Cancelled);
    }
    if Instant::now() >= context.deadline {
        return Err(AgentError::TimedOut);
    }
    Ok(())
}

fn drive_turn(
    input: &mut io::Take<impl BufRead>,
    output: &mut impl Write,
    context: AgentExecutionContext<'_>,
    prompt: &str,
    action: TurnAction<'_>,
    dispatched: &mut bool,
) -> Result<TurnResult> {
    check_context(context)?;
    if prompt.len() > MAX_REQUEST {
        return Err(AgentError::Rejected("Codex prompt exceeds input limit".into()).into());
    }
    send(
        output,
        &json!({
            "method": "initialize",
            "id": 1,
            "params": {"clientInfo": {"name": "mcbtodo", "version": env!("CARGO_PKG_VERSION")}}
        }),
    )?;
    wait_for_reply(input, 1, context, |_| {}).context("wait for initialize reply")?;
    send(output, &json!({"method": "initialized"}))?;
    send(
        output,
        &json!({
            "method": "thread/start",
            "id": 2,
            "params": {
                "cwd": "/scratch",
                "ephemeral": true,
                "approvalPolicy": "never",
                "sandbox": "read-only"
            }
        }),
    )?;
    let thread =
        wait_for_reply(input, 2, context, |_| {}).context("wait for thread/start reply")?;
    let thread_id = restricted_thread_id(&thread)?;
    let schema = match action {
        TurnAction::Interrupt => &Value::Null,
        TurnAction::Structured(schema) => schema,
    };
    let request = encode(&json!({
        "method": "turn/start",
        "id": 3,
        "params": {
            "threadId": thread_id,
            "input": [{"type": "text", "text": prompt}],
            "outputSchema": schema
        }
    }))?;
    check_context(context)?;
    *dispatched = true;
    output
        .write_all(&request.0[..request.1])
        .context("send turn/start request")?;
    // The stream can start before the response arrives. Keep its prefix and
    // subject those early notifications to the same correlation checks.
    let mut pending = VecDeque::new();
    let turn = wait_for_reply(input, 3, context, |message| pending.push_back(message))
        .context("wait for turn/start reply")?;
    let turn_id = turn["turn"]["id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .context("missing Codex turn ID")?;
    ensure!(
        turn["turn"]["status"] == "inProgress",
        "Codex did not start a turn"
    );
    let mut next = |context| {
        check_context(context)?;
        pending
            .pop_front()
            .map_or_else(|| read_frame(input, context), Ok)
    };
    match action {
        TurnAction::Structured(_) => {
            let result = read_streamed_output(&mut || next(context), thread_id, turn_id);
            if matches!(
                &result,
                Err(error) if matches!(error.downcast_ref::<AgentError>(), Some(AgentError::Cancelled))
            ) {
                // This control exchange uses the remainder of the host deadline.
                // The call stays uncertain even if the server acknowledges.
                let cleanup = AgentExecutionContext {
                    deadline: context.deadline.min(Instant::now() + INTERRUPT_GRACE),
                    cancellation: &AtomicBool::new(false),
                };
                if check_context(cleanup).is_ok() {
                    let _ = interrupt_turn(output, &mut || next(cleanup), thread_id, turn_id);
                }
            }
            result.map(TurnResult::Structured)
        }
        TurnAction::Interrupt => {
            check_context(context)?;
            interrupt_turn(output, &mut || next(context), thread_id, turn_id)?;
            Ok(TurnResult::Interrupted)
        }
    }
}

fn send(output: &mut impl Write, request: &Value) -> Result<()> {
    // Each Linux nonblocking pipe write fits in PIPE_BUF.
    let (bytes, len) = encode(request)?;
    output
        .write_all(&bytes[..len])
        .context("send Codex request")
}

fn encode(request: &Value) -> Result<([u8; MAX_REQUEST], usize), AgentError> {
    let mut bytes = [0; MAX_REQUEST];
    let mut buffer = Cursor::new(bytes.as_mut_slice());
    serde_json::to_writer(&mut buffer, request)
        .and_then(|()| buffer.write_all(b"\n").map_err(serde_json::Error::io))
        .map_err(|_| AgentError::Rejected("Codex request exceeds frame limit".into()))?;
    let len = buffer.position() as usize;
    Ok((bytes, len))
}

fn wait_for_reply(
    input: &mut io::Take<impl BufRead>,
    id: u64,
    context: AgentExecutionContext<'_>,
    mut notify: impl FnMut(Value),
) -> Result<Value> {
    for _ in 0..MAX_CONTROL_MESSAGES {
        let message = read_frame(input, context)?;
        if message.get("id").is_none() && message["method"].is_string() {
            notify(message);
            continue;
        }
        return check_reply(message, id);
    }
    bail!("Codex notification limit exceeded during request {id}")
}

fn read_frame(
    input: &mut io::Take<impl BufRead>,
    context: AgentExecutionContext<'_>,
) -> Result<Value> {
    let mut line = Vec::new();
    loop {
        check_context(context)?;
        ensure!(input.limit() > 0, "Codex session response limit exceeded");
        let read = (&mut *input)
            .take((MAX_FRAME + 1 - line.len()) as u64)
            .read_until(b'\n', &mut line);
        ensure!(
            line.len() <= MAX_FRAME,
            "Codex response exceeds frame limit"
        );
        match read {
            Ok(0) => bail!("Codex closed its response stream"),
            Ok(_) => {
                ensure!(
                    input.limit() > 0 || line.ends_with(b"\n"),
                    "Codex session response limit exceeded"
                );
                ensure!(line.ends_with(b"\n"), "incomplete Codex JSONL frame");
                return serde_json::from_slice(&line)
                    .map_err(|_| anyhow::anyhow!("invalid Codex JSONL frame"));
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(
                    POLL_INTERVAL.min(context.deadline.saturating_duration_since(Instant::now())),
                );
            }
            Err(error) => return Err(error).context("read Codex response"),
        }
    }
}

fn check_reply(message: Value, id: u64) -> Result<Value> {
    ensure!(
        message["id"] == id && message.get("method").is_none(),
        "unexpected Codex message during request {id}"
    );
    ensure!(
        message.get("error").is_none(),
        "Codex rejected request {id}"
    );
    message
        .get("result")
        .cloned()
        .context("Codex response lacks result")
}

fn interrupt_turn(
    output: &mut impl Write,
    next: &mut impl FnMut() -> Result<Value>,
    thread_id: &str,
    turn_id: &str,
) -> Result<()> {
    send(
        output,
        &json!({
            "method": "turn/interrupt", "id": 4,
            "params": {"threadId": thread_id, "turnId": turn_id}
        }),
    )?;
    await_interruption(next, thread_id, turn_id)
}

fn await_interruption(
    next: &mut impl FnMut() -> Result<Value>,
    thread_id: &str,
    turn_id: &str,
) -> Result<()> {
    let mut acknowledged = false;
    let mut interrupted = false;
    for _ in 0..MAX_CONTROL_MESSAGES {
        let message = next()?;
        match (message.get("id"), message["method"].as_str()) {
            (None, Some("turn/completed")) => {
                let params = &message["params"];
                ensure!(
                    !interrupted
                        && params["threadId"] == thread_id
                        && params["turn"]["id"] == turn_id
                        && params["turn"]["status"] == "interrupted",
                    "Codex did not confirm interruption of the turn"
                );
                interrupted = true;
            }
            (None, Some(_)) => {}
            _ => {
                ensure!(
                    !acknowledged,
                    "unexpected Codex message after interrupt reply"
                );
                check_reply(message, 4)?;
                acknowledged = true;
            }
        }
        if acknowledged && interrupted {
            return Ok(());
        }
    }
    bail!("Codex notification limit exceeded during interruption")
}

fn read_streamed_output(
    next: &mut impl FnMut() -> Result<Value>,
    thread_id: &str,
    turn_id: &str,
) -> Result<AgentResult> {
    let mut item_id = None;
    let mut streamed = String::new();
    let mut item_completed = false;
    for _ in 0..MAX_STREAM_MESSAGES {
        let message = next()?;
        ensure!(
            message.get("id").is_none(),
            "unexpected Codex server request or reply"
        );
        let params = &message["params"];
        if let Some(id) = params.get("threadId") {
            ensure!(
                id == thread_id,
                "notification belongs to a different thread"
            );
        }
        if let Some(id) = params.get("turnId") {
            ensure!(id == turn_id, "notification belongs to a different turn");
        }
        match message["method"].as_str() {
            Some("item/agentMessage/delta") => {
                ensure!(
                    params["threadId"] == thread_id
                        && params["turnId"] == turn_id
                        && !item_completed,
                    "out-of-order or uncorrelated Codex output delta"
                );
                let id = params["itemId"]
                    .as_str()
                    .context("missing Codex output item")?;
                ensure!(
                    item_id.get_or_insert_with(|| id.to_owned()) == id,
                    "Codex streamed multiple output items"
                );
                let delta = params["delta"]
                    .as_str()
                    .context("missing Codex text delta")?;
                ensure!(
                    streamed.len() + delta.len() <= MAX_OUTPUT,
                    "Codex output exceeds limit"
                );
                streamed.push_str(delta);
            }
            Some("item/completed") if params["item"]["type"] == "agentMessage" => {
                ensure!(
                    params["threadId"] == thread_id
                        && params["turnId"] == turn_id
                        && item_id.as_deref() == params["item"]["id"].as_str()
                        && item_id.is_some()
                        && !item_completed,
                    "unexpected Codex assistant result"
                );
                let text = params["item"]["text"]
                    .as_str()
                    .context("missing Codex assistant text")?;
                ensure!(
                    text == streamed && !text.is_empty(),
                    "stream and final result differ"
                );
                item_completed = true;
            }
            Some("turn/completed") => {
                ensure!(
                    params["threadId"] == thread_id
                        && params["turn"]["id"] == turn_id
                        && params["turn"]["status"] == "completed",
                    "Codex did not complete the turn"
                );
                ensure!(
                    item_completed,
                    "Codex turn completed without a final assistant item"
                );
                return parse_outputs(&streamed);
            }
            Some(_) => {}
            None => bail!("Codex sent a message without a notification method"),
        }
    }
    bail!("Codex notification limit exceeded while streaming")
}

fn parse_outputs(text: &str) -> Result<AgentResult> {
    let mut deserializer = serde_json::Deserializer::from_str(text);
    // Do not include parser diagnostics that can echo model output.
    AgentResult::deserialize_outputs(&mut deserializer)
        .and_then(|outputs| {
            deserializer.end()?;
            Ok(AgentResult { outputs })
        })
        .map_err(|_| {
            anyhow::anyhow!("Codex result must be a JSON object with unique scalar outputs")
        })
}

fn restricted_thread_id(result: &Value) -> Result<&str> {
    ensure!(
        result["approvalPolicy"] == "never"
            && result["sandbox"]["type"] == "readOnly"
            && result["sandbox"]["networkAccess"] == false
            && result["cwd"] == "/scratch"
            && result["thread"]["ephemeral"] == true,
        "Codex thread did not confirm the requested restrictive settings"
    );
    result["thread"]["id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .context("missing Codex thread ID")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    const FIXTURE_OUTPUT: &str = r#"{"title":"已验证 streamed reply"}"#;
    const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
    static RUNNING: AtomicBool = AtomicBool::new(false);

    fn test_context() -> AgentExecutionContext<'static> {
        AgentExecutionContext {
            deadline: Instant::now() + PROBE_TIMEOUT,
            cancellation: &RUNNING,
        }
    }

    fn reply(input: &mut impl BufRead, id: u64) -> Result<Value> {
        wait_for_reply(
            &mut input.take(MAX_SESSION_RESPONSE),
            id,
            test_context(),
            |_| {},
        )
    }

    #[test]
    fn skips_notifications_and_correlates_replies() {
        let mut wire = Cursor::new(b"{\"method\":\"notice\"}\n{\"id\":2,\"result\":{}}\n");
        assert_eq!(reply(&mut wire, 2).unwrap(), json!({}));
    }

    #[test]
    fn fails_closed_on_invalid_frames_and_unexpected_messages() {
        for frame in [
            b"{\"id\":2,\"result\":{}}".as_slice(),
            b"{broken json}\n",
            b"{\"id\":3,\"result\":{}}\n",
            b"{\"id\":2,\"method\":\"server/request\"}\n",
            b"{\"id\":2,\"error\":{},\"result\":{}}\n",
            &vec![b'x'; MAX_FRAME + 1],
        ] {
            assert!(reply(&mut Cursor::new(frame), 2).is_err());
        }
    }

    #[test]
    fn interruption_requires_correlated_acknowledgment_and_terminal_status() {
        let ack = "{\"id\":4,\"result\":{}}\n";
        let terminal = |thread_id, turn_id, status| {
            format!(
                "{}\n",
                json!({
                    "method": "turn/completed",
                    "params": {"threadId": thread_id, "turn": {"id": turn_id, "status": status}}
                })
            )
        };
        let valid = terminal("thread", "turn", "interrupted");
        let check = |frames: String| {
            let mut frames = Cursor::new(frames.into_bytes()).take(MAX_SESSION_RESPONSE);
            await_interruption(
                &mut || read_frame(&mut frames, test_context()),
                "thread",
                "turn",
            )
        };
        assert!(check(format!("{ack}{valid}")).is_ok());
        assert!(check(format!("{valid}{ack}")).is_ok());
        assert!(check(ack.into()).is_err());
        assert!(check(valid).is_err());
        for invalid in [
            terminal("other", "turn", "interrupted"),
            terminal("thread", "other", "interrupted"),
            terminal("thread", "turn", "completed"),
        ] {
            assert!(check(format!("{ack}{invalid}")).is_err());
        }
    }

    #[test]
    fn fixture_result_requires_bounded_correlated_stream_and_final_item() {
        let notification =
            |method, params| format!("{}\n", json!({"method": method, "params": params}));
        let delta = |thread: &str, item: &str, text: &str| {
            notification(
                "item/agentMessage/delta",
                json!({"threadId": thread, "turnId": "turn", "itemId": item, "delta": text}),
            )
        };
        let item = |text| {
            notification(
                "item/completed",
                json!({"threadId": "thread", "turnId": "turn",
                    "item": {"id": "item", "type": "agentMessage", "text": text}}),
            )
        };
        let terminal = |status| {
            notification(
                "turn/completed",
                json!({"threadId": "thread", "turn": {"id": "turn", "status": status}}),
            )
        };
        let first = delta("thread", "item", r#"{"title":"已验证"#);
        let second = delta("thread", "item", r#" streamed reply"}"#);
        let final_item = item(FIXTURE_OUTPUT);
        let completed = terminal("completed");
        let check = |frames: String| {
            let mut frames = Cursor::new(frames.into_bytes()).take(MAX_SESSION_RESPONSE);
            read_streamed_output(
                &mut || read_frame(&mut frames, test_context()),
                "thread",
                "turn",
            )
        };
        assert!(check(format!("{first}{second}{final_item}{completed}")).is_ok());
        for frames in [
            format!("{first}{second}{completed}"),
            format!("{first}{second}{final_item}"),
            format!("{first}{second}{final_item}{}", terminal("interrupted")),
            format!("{first}{second}{final_item}{final_item}{completed}"),
            format!(
                "{first}{second}{}{completed}",
                item(r#"{"title":"different"}"#)
            ),
            format!(
                "{}{second}{final_item}{completed}",
                delta("other", "item", FIXTURE_OUTPUT)
            ),
            format!(
                "{first}{}{final_item}{completed}",
                delta("thread", "other", FIXTURE_OUTPUT)
            ),
        ] {
            assert!(check(frames).is_err());
        }
        let oversized = check(delta("thread", "item", &"界".repeat(6000))).unwrap_err();
        assert_eq!(oversized.to_string(), "Codex output exceeds limit");
        let duplicate = r#"{"title":"one","title":"two"}"#;
        assert!(
            check(format!(
                "{}{}{completed}",
                delta("thread", "item", duplicate),
                item(duplicate)
            ))
            .is_err(),
            "a duplicate output field must not be normalized into a valid result"
        );
    }

    fn handshake() -> [Value; 2] {
        [
            json!({"id": 1, "result": {"userAgent": "mcbtodo/99.0.0 example"}}),
            json!({"id": 2, "result": {
                "approvalPolicy": "never", "sandbox": {"type": "readOnly", "networkAccess": false},
                "cwd": "/scratch", "thread": {"ephemeral": true, "id": "thread"}
            }}),
        ]
    }

    fn wire(frames: impl IntoIterator<Item = Value>) -> Cursor<Vec<u8>> {
        Cursor::new(
            frames
                .into_iter()
                .map(|value| format!("{value}\n"))
                .collect::<String>()
                .into_bytes(),
        )
    }

    #[test]
    fn retains_result_delta_from_before_turn_start_reply() {
        let params = json!({"threadId": "thread", "turnId": "turn", "itemId": "item", "delta": FIXTURE_OUTPUT});
        let mut input = wire(handshake().into_iter().chain([
            json!({"method": "item/agentMessage/delta", "params": params}),
            json!({"id": 3, "result": {"turn": {"id": "turn", "status": "inProgress"}}}),
            json!({"method": "item/completed", "params": {
                "threadId": "thread", "turnId": "turn", "item": {"type": "agentMessage", "id": "item", "text": FIXTURE_OUTPUT}
            }}),
            json!({"method": "turn/completed", "params": {
                "threadId": "thread", "turn": {"id": "turn", "status": "completed"}
            }}),
        ]));
        let result = run_turn(
            &mut input,
            &mut Vec::new(),
            test_context(),
            "fixture",
            TurnAction::Structured(&json!({})),
        )
        .unwrap();
        assert_eq!(
            result,
            TurnResult::Structured(AgentResult {
                outputs: [(
                    "title".into(),
                    mcbtodo_ports::Scalar::String("已验证 streamed reply".into()),
                )]
                .into(),
            })
        );
    }

    #[test]
    fn accepts_many_small_deltas_without_expanding_the_handshake_limit() {
        let text = json!({"title": "跨平台计划".repeat(18)}).to_string();
        let deltas = text.chars().map(|ch| {
            json!({"method": "item/agentMessage/delta", "params": {
                "threadId": "thread", "turnId": "turn", "itemId": "item", "delta": ch.to_string()
            }})
        });
        let mut input = wire(
            handshake()
                .into_iter()
                .chain([
                    json!({"id": 3, "result": {"turn": {"id": "turn", "status": "inProgress"}}}),
                ])
                .chain(deltas)
                .chain([
                    json!({"method": "item/completed", "params": {
                        "threadId": "thread", "turnId": "turn",
                        "item": {"type": "agentMessage", "id": "item", "text": text}
                    }}),
                    json!({"method": "turn/completed", "params": {
                        "threadId": "thread", "turn": {"id": "turn", "status": "completed"}
                    }}),
                ]),
        );
        let run = |input: &mut Cursor<Vec<u8>>, writes: &mut Vec<u8>| {
            run_turn(
                input,
                writes,
                test_context(),
                "fixture",
                TurnAction::Structured(&json!({})),
            )
        };
        assert_eq!(
            run(&mut input, &mut Vec::new()).unwrap(),
            TurnResult::Structured(AgentResult {
                outputs: [(
                    "title".into(),
                    mcbtodo_ports::Scalar::String("跨平台计划".repeat(18)),
                )]
                .into(),
            })
        );

        let noise = std::iter::repeat_n(json!({"method": "notice"}), MAX_CONTROL_MESSAGES);
        let mut writes = Vec::new();
        assert!(
            matches!(run(&mut wire(noise.chain(handshake())), &mut writes),
                Err(AgentError::Transport(message)) if message.contains("notification limit"))
        );
        assert!(!String::from_utf8(writes).unwrap().contains("turn/start"));
    }

    #[test]
    fn bounds_aggregate_stdout_before_parsing_the_next_frame() {
        // Individually valid frames, with enough payload to exhaust the whole
        // session without reaching even the old 32-message streaming cap.
        let noise = json!({"method": "notice", "params": {"padding": "x".repeat(60 * 1024)}});
        let frames = || std::iter::repeat_n(noise.clone(), 20);
        let invoke = |input: &mut Cursor<Vec<u8>>| {
            run_turn(
                input,
                &mut Vec::new(),
                test_context(),
                "fixture",
                TurnAction::Structured(&json!({})),
            )
        };
        let mut before_start = wire(frames().chain(handshake()));
        assert!(matches!(invoke(&mut before_start),
                Err(AgentError::Transport(message)) if message.contains("session response limit")));
        assert_eq!(before_start.position(), 1024 * 1024);

        let mut after_start = wire(
            handshake()
                .into_iter()
                .chain([
                    json!({"id": 3, "result": {"turn": {"id": "turn", "status": "inProgress"}}}),
                ])
                .chain(frames()),
        );
        assert!(matches!(invoke(&mut after_start),
                Err(AgentError::OutcomeUnknown(message)) if message.contains("session response limit")));
        assert_eq!(after_start.position(), 1024 * 1024);
    }

    #[test]
    fn rejected_initialize_stops_before_thread_start() {
        let mut frames = handshake();
        frames[0] = json!({"id": 1, "error": {"code": -32603, "message": "unavailable"}});
        let mut writes = Vec::new();
        assert!(matches!(
            run_turn(
                &mut wire(frames),
                &mut writes,
                test_context(),
                "fixture",
                TurnAction::Interrupt
            ),
            Err(AgentError::Transport(message)) if message.contains("Codex rejected request 1")
        ));
        let writes = String::from_utf8(writes).unwrap();
        assert!(!writes.contains("\"method\":\"initialized\""));
        assert!(!writes.contains("thread/start"));
    }

    #[test]
    fn does_not_dispatch_when_sandbox_network_is_enabled() {
        let mut frames = handshake();
        frames[1]["result"]["sandbox"]["networkAccess"] = json!(true);
        let mut writes = Vec::new();
        assert!(matches!(
            run_turn(
                &mut wire(frames),
                &mut writes,
                test_context(),
                "fixture",
                TurnAction::Interrupt
            ),
            Err(AgentError::Transport(_))
        ));
        assert!(!String::from_utf8(writes).unwrap().contains("turn/start"));
    }

    #[test]
    fn cancellation_and_process_exit_preserve_the_dispatch_boundary() {
        struct CancelOnDispatch<'a>(&'a AtomicBool);

        impl Write for CancelOnDispatch<'_> {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                let request: Value = serde_json::from_slice(bytes).unwrap();
                if request["method"] == "turn/start" {
                    self.0.store(true, Ordering::Release);
                }
                Ok(bytes.len())
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let cancel = AtomicBool::new(true);
        let context = AgentExecutionContext {
            cancellation: &cancel,
            ..test_context()
        };
        let mut writes = Vec::new();
        assert!(matches!(
            run_turn(
                &mut wire(handshake()),
                &mut writes,
                context,
                "fixture",
                TurnAction::Interrupt
            ),
            Err(AgentError::Cancelled)
        ));
        assert!(writes.is_empty());
        cancel.store(false, Ordering::Release);
        assert!(
            matches!(run_turn(&mut wire(handshake()), &mut CancelOnDispatch(&cancel), context, "fixture", TurnAction::Interrupt),
            Err(AgentError::OutcomeUnknown(message)) if message.contains("cancelled"))
        );
        assert!(
            matches!(run_turn(&mut wire(handshake()), &mut writes, test_context(), "fixture", TurnAction::Interrupt),
            Err(AgentError::OutcomeUnknown(message)) if message.contains("closed its response stream"))
        );
    }

    #[test]
    fn cancellation_of_acknowledged_turn_interrupts_without_reclassifying_the_outcome() {
        struct CancelOnAck<'a> {
            input: Cursor<Vec<u8>>,
            offset: u64,
            cancelled: &'a AtomicBool,
        }

        impl Read for CancelOnAck<'_> {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                self.input.read(bytes)
            }
        }

        impl BufRead for CancelOnAck<'_> {
            fn fill_buf(&mut self) -> io::Result<&[u8]> {
                self.input.fill_buf()
            }

            fn consume(&mut self, bytes: usize) {
                self.input.consume(bytes);
                if self.input.position() >= self.offset {
                    self.cancelled.store(true, Ordering::Release);
                }
            }
        }

        let mut wire =
            wire(handshake().into_iter().chain([
                json!({"id": 3, "result": {"turn": {"id": "turn", "status": "inProgress"}}}),
            ]));
        let cancel_offset = wire.get_ref().len() as u64;
        writeln!(wire.get_mut(), "{}", json!({"id": 4, "result": {}})).unwrap();
        writeln!(
            wire.get_mut(),
            "{}",
            json!({"method": "turn/completed", "params": {
                "threadId": "thread",
                "turn": {"id": "turn", "status": "interrupted"}
            }})
        )
        .unwrap();
        let cancelled = AtomicBool::new(false);
        let mut input = CancelOnAck {
            input: wire,
            offset: cancel_offset,
            cancelled: &cancelled,
        };
        let mut writes = Vec::new();
        assert!(matches!(
            run_turn(
                &mut input,
                &mut writes,
                AgentExecutionContext {
                    cancellation: &cancelled,
                    ..test_context()
                },
                "fixture",
                TurnAction::Structured(&json!({})),
            ),
            Err(AgentError::OutcomeUnknown(message)) if message.contains("cancelled")
        ));
        let interrupt: Value = serde_json::from_slice(
            writes
                .split(|byte| *byte == b'\n')
                .rfind(|line| !line.is_empty())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            interrupt,
            json!({"method": "turn/interrupt", "id": 4, "params": {"threadId": "thread", "turnId": "turn"}})
        );
        assert_eq!(input.input.position(), input.input.get_ref().len() as u64);
    }
}
