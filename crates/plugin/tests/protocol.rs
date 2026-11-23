use std::{
    io::{self, Cursor, Read, Write},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

use mcbtodo_plugin::{
    HostMessage, MAX_FRAME_BYTES, PluginMessage, invoke, read_host_message, read_plugin_message,
    write_host_message, write_plugin_message,
};
use mcbtodo_ports::{AgentError, AgentExecutionContext, AgentRequest, Scalar};

const HELLO: &str = r#"{"type":"hello","version":1}"#;
const READY: &str = r#"{"type":"ready","version":1,"capabilities":["summary"]}"#;
const EXECUTE: &str =
    r#"{"type":"execute","id":1,"capability":"summary","inputs":{"text":"example"}}"#;
const RESULT: &str = r#"{"type":"result","id":1,"outputs":{"accepted":true,"count":-42}}"#;

fn frame(json: &str) -> Vec<u8> {
    [
        (json.len() as u32).to_be_bytes().as_slice(),
        json.as_bytes(),
    ]
    .concat()
}

fn request() -> AgentRequest {
    AgentRequest {
        capability: "summary".into(),
        revision: "host-only-revision".into(),
        inputs: [("text".into(), Scalar::String("example".into()))].into(),
    }
}

fn context(cancellation: &AtomicBool, timeout: Duration) -> AgentExecutionContext<'_> {
    AgentExecutionContext {
        deadline: Instant::now() + timeout,
        cancellation,
    }
}

#[test]
fn exchange_uses_the_v1_wire_contract_and_omits_revision() {
    let input = [frame(READY), frame(RESULT)].concat();
    let mut read = input.as_slice();
    let mut written = Vec::new();
    let result = invoke(
        &mut read,
        &mut written,
        request(),
        context(&AtomicBool::new(false), Duration::from_secs(2)),
    )
    .unwrap();
    assert_eq!(written, [frame(HELLO), frame(EXECUTE)].concat());
    assert_eq!(
        result.outputs,
        [
            ("accepted".into(), Scalar::Boolean(true)),
            ("count".into(), Scalar::Integer(-42)),
        ]
        .into()
    );
}

#[test]
fn blocking_helpers_interoperate_at_the_exact_size_limit() {
    let host = HostMessage::Hello { version: 1 };
    let mut bytes = Vec::new();
    write_host_message(&mut bytes, &host).unwrap();
    assert_eq!(bytes, frame(HELLO));
    assert_eq!(read_host_message(&mut bytes.as_slice()).unwrap(), host);

    let mut result = PluginMessage::Result {
        id: 1,
        outputs: [("text".into(), Scalar::String(String::new()))].into(),
    };
    bytes.clear();
    write_plugin_message(&mut bytes, &result).unwrap();
    let PluginMessage::Result { outputs, .. } = &mut result else {
        unreachable!()
    };
    outputs.insert(
        "text".into(),
        Scalar::String("x".repeat(MAX_FRAME_BYTES + 4 - bytes.len())),
    );
    bytes.clear();
    write_plugin_message(&mut bytes, &result).unwrap();
    assert_eq!(bytes.len(), MAX_FRAME_BYTES + 4);
    assert_eq!(read_plugin_message(&mut bytes.as_slice()).unwrap(), result);
    let PluginMessage::Result { outputs, .. } = &mut result else {
        unreachable!()
    };
    let Some(Scalar::String(text)) = outputs.get_mut("text") else {
        unreachable!()
    };
    text.push('x');
    bytes.clear();
    assert!(write_plugin_message(&mut bytes, &result).is_err());
    assert!(bytes.is_empty());

    for size in [0, MAX_FRAME_BYTES as u32 + 1, u32::MAX] {
        let mut reader = Cursor::new([size.to_be_bytes().as_slice(), b"untouched"].concat());
        let err = read_plugin_message(&mut reader).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert_eq!(reader.position(), 4);
    }
}

#[test]
fn bad_handshake_stops_before_dispatch() {
    for reply in [
        r#"{"type":"ready","version":2,"capabilities":["summary"]}"#,
        r#"{"type":"ready","version":1,"capabilities":[]}"#,
        r#"{"type":"ready","version":1,"capabilities":["summary"],"extra":true}"#,
        r#"{"type":"ready","version":1,"version":1,"capabilities":["summary"]}"#,
        r#"{"type":"ready","type":"ready","version":1,"capabilities":["summary"]}"#,
        RESULT,
    ] {
        let mut written = Vec::new();
        let err = invoke(
            &mut frame(reply).as_slice(),
            &mut written,
            request(),
            context(&AtomicBool::new(false), Duration::from_secs(2)),
        )
        .unwrap_err();
        assert!(!matches!(err, AgentError::OutcomeUnknown(_)), "{reply}");
        assert_eq!(written, frame(HELLO));
    }
    for len in [0, MAX_FRAME_BYTES as u32 + 1] {
        let mut written = Vec::new();
        let err = invoke(
            &mut len.to_be_bytes().as_slice(),
            &mut written,
            request(),
            context(&AtomicBool::new(false), Duration::from_secs(2)),
        )
        .unwrap_err();
        assert!(matches!(err, AgentError::Transport(_)));
        assert_eq!(written, frame(HELLO));
    }
}

#[test]
fn malformed_or_mismatched_result_is_unknown() {
    for reply in [
        "",
        READY,
        r#"{"type":"result","id":2,"outputs":{}}"#,
        r#"{"type":"result","id":1,"id":1,"outputs":{}}"#,
        r#"{"type":"result","id":1.0,"outputs":{}}"#,
        r#"{"type":"result","id":1,"outputs":{},"extra":true}"#,
        r#"{"type":"result","id":1,"outputs":{"a":1,"\u0061":2}}"#,
        r#"{"type":"result","id":1,"outputs":{"a":9223372036854775808}}"#,
        r#"{"type":"result","id":1,"outputs":{"a":0.0}}"#,
        r#"{"type":"result","id":1,"outputs":{"a":null}}"#,
        r#"{"type":"result","id":1,"outputs":{"a":[]}}"#,
        r#"{"type":"result","id":1,"outputs":{"a":{}}}"#,
        r#"{"type":"result","id":1,"outputs":{}} {}"#,
        r#"["result",1,{}]"#,
    ] {
        let err = invoke(
            &mut [frame(READY), frame(reply)].concat().as_slice(),
            &mut Vec::new(),
            request(),
            context(&AtomicBool::new(false), Duration::from_secs(2)),
        )
        .unwrap_err();
        assert!(matches!(err, AgentError::OutcomeUnknown(_)), "{reply}");
    }
    for json in [
        r#"{"type":"execute","id":1,"capability":"summary","inputs":{"a":true,"a":false}}"#,
        r#"{"type":"execute","id":1,"capability":"summary","inputs":{},"revision":"x"}"#,
    ] {
        assert!(read_host_message(&mut frame(json).as_slice()).is_err());
    }
}

#[test]
fn oversized_request_stops_before_dispatch() {
    let mut request = request();
    request.capability = "summary".into();
    request
        .inputs
        .insert("text".into(), Scalar::String("x".repeat(MAX_FRAME_BYTES)));
    let mut written = Vec::new();
    let err = invoke(
        &mut frame(READY).as_slice(),
        &mut written,
        request,
        context(&AtomicBool::new(false), Duration::from_secs(2)),
    )
    .unwrap_err();
    assert!(matches!(err, AgentError::Rejected(_)));
    assert_eq!(written, frame(HELLO));
}

struct Chunks<T> {
    stream: T,
    calls: usize,
}

impl<T> Chunks<T> {
    fn new(stream: T) -> Self {
        Self { stream, calls: 0 }
    }

    fn next(&mut self) -> io::Result<()> {
        self.calls += 1;
        match self.calls % 8 {
            1 => Err(io::ErrorKind::WouldBlock.into()),
            2 => Err(io::ErrorKind::Interrupted.into()),
            _ => Ok(()),
        }
    }
}

impl<T: Read> Read for Chunks<T> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.next()?;
        let len = bytes.len().min(3);
        self.stream.read(&mut bytes[..len])
    }
}

impl<T: Write> Write for Chunks<T> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.next()?;
        self.stream.write(&bytes[..bytes.len().min(3)])
    }

    fn flush(&mut self) -> io::Result<()> {
        self.next()?;
        self.stream.flush()
    }
}

#[test]
fn partial_io_retries_without_losing_progress() {
    let mut input = Chunks::new(Cursor::new([frame(READY), frame(RESULT)].concat()));
    let mut output = Chunks::new(Vec::new());
    invoke(
        &mut input,
        &mut output,
        request(),
        context(&AtomicBool::new(false), Duration::from_secs(2)),
    )
    .unwrap();
    assert_eq!(output.stream, [frame(HELLO), frame(EXECUTE)].concat());
}

struct Stall<'a> {
    prefix: &'a [u8],
    cancel: Option<&'a AtomicBool>,
}

impl Read for Stall<'_> {
    fn read(&mut self, next: &mut [u8]) -> io::Result<usize> {
        if !self.prefix.is_empty() {
            return self.prefix.read(next);
        }
        if let Some(cancel) = self.cancel {
            cancel.store(true, Ordering::Release);
        }
        Err(io::ErrorKind::WouldBlock.into())
    }
}

#[test]
fn partial_reads_honor_deadline_and_cancellation_in_each_phase() {
    for dispatched in [false, true] {
        for cancel in [false, true] {
            let cancelled = AtomicBool::new(false);
            let bytes = if dispatched {
                [frame(READY), frame(RESULT)[..6].to_vec()].concat()
            } else {
                frame(READY)[..2].to_vec()
            };
            let mut input = Stall {
                prefix: &bytes,
                cancel: cancel.then_some(&cancelled),
            };
            let timeout = if cancel {
                Duration::from_secs(2)
            } else {
                Duration::from_millis(40)
            };
            let err = invoke(
                &mut input,
                &mut Vec::new(),
                request(),
                context(&cancelled, timeout),
            )
            .unwrap_err();
            assert!(match (dispatched, cancel) {
                (false, false) => matches!(err, AgentError::TimedOut),
                (false, true) => matches!(err, AgentError::Cancelled),
                (true, _) => matches!(err, AgentError::OutcomeUnknown(_)),
            });
        }
    }
}

struct StopWriting<'a> {
    remaining: usize,
    error: io::ErrorKind,
    cancel: Option<&'a AtomicBool>,
}

impl Write for StopWriting<'_> {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            if let Some(cancel) = self.cancel {
                cancel.store(true, Ordering::Release);
            }
            return Err(self.error.into());
        }
        let count = self.remaining.min(input.len());
        self.remaining -= count;
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn first_execute_write_is_the_uncertainty_boundary() {
    let hello_size = frame(HELLO).len();
    for (remaining, kind, cancel, dispatched) in [
        (1, io::ErrorKind::WouldBlock, false, false),
        (1, io::ErrorKind::WouldBlock, true, false),
        (hello_size, io::ErrorKind::BrokenPipe, false, false),
        (hello_size, io::ErrorKind::WouldBlock, false, false),
        (hello_size, io::ErrorKind::WouldBlock, true, false),
        (hello_size + 2, io::ErrorKind::WouldBlock, true, true),
    ] {
        let cancelled = AtomicBool::new(false);
        let mut output = StopWriting {
            remaining,
            error: kind,
            cancel: cancel.then_some(&cancelled),
        };
        let timeout = if kind == io::ErrorKind::WouldBlock && !cancel {
            Duration::from_millis(40)
        } else {
            Duration::from_secs(2)
        };
        let err = invoke(
            &mut frame(READY).as_slice(),
            &mut output,
            request(),
            context(&cancelled, timeout),
        )
        .unwrap_err();
        assert!(
            match (dispatched, kind, cancel) {
                (false, io::ErrorKind::BrokenPipe, _) => matches!(err, AgentError::Transport(_)),
                (false, _, false) => matches!(err, AgentError::TimedOut),
                (false, _, true) => matches!(err, AgentError::Cancelled),
                (true, _, _) => matches!(err, AgentError::OutcomeUnknown(_)),
            },
            "{err:?}"
        );
    }
}
