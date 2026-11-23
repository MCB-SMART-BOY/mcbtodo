//! A small, versioned, framed-JSON protocol for mcbtodo Agent plugins.

use std::{
    io::{self, Read, Write},
    time::{Duration, Instant},
};

use mcbtodo_ports::{AgentError, AgentExecutionContext, AgentRequest, AgentResult, Scalar};
use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u16 = 1;
pub const MAX_FRAME_BYTES: usize = 16 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(5);

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum HostMessage {
    #[serde(rename = "hello")]
    Hello { version: u16 },
    #[serde(rename = "execute")]
    Execute {
        id: u64,
        capability: String,
        #[serde(deserialize_with = "AgentResult::deserialize_outputs")]
        inputs: std::collections::BTreeMap<String, Scalar>,
    },
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum PluginMessage {
    #[serde(rename = "ready")]
    Ready {
        version: u16,
        capabilities: Vec<String>,
    },
    #[serde(rename = "result")]
    Result {
        id: u64,
        #[serde(deserialize_with = "AgentResult::deserialize_outputs")]
        outputs: std::collections::BTreeMap<String, Scalar>,
    },
}

fn invalid(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

fn encode_frame(value: impl Serialize) -> io::Result<Vec<u8>> {
    let mut frame = vec![0; 4];
    serde_json::to_writer(&mut frame, &value).map_err(invalid)?;
    let length = frame.len() - 4;
    if length > MAX_FRAME_BYTES {
        return Err(invalid("plugin frame exceeds limit"));
    }
    frame[..4].copy_from_slice(&(length as u32).to_be_bytes());
    Ok(frame)
}

fn write_frame(output: &mut impl Write, value: impl Serialize) -> io::Result<()> {
    output.write_all(&encode_frame(value)?)?;
    output.flush()
}

fn frame_length(length: [u8; 4]) -> io::Result<usize> {
    let length = u32::from_be_bytes(length) as usize;
    if !(1..=MAX_FRAME_BYTES).contains(&length) {
        return Err(invalid("plugin frame exceeds limit"));
    }
    Ok(length)
}

fn read_frame(input: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut length = [0; 4];
    input.read_exact(&mut length)?;
    let mut body = vec![0; frame_length(length)?];
    input.read_exact(&mut body)?;
    Ok(body)
}

pub fn write_host_message(output: &mut impl Write, message: &HostMessage) -> io::Result<()> {
    write_frame(output, message)
}
pub fn write_plugin_message(output: &mut impl Write, message: &PluginMessage) -> io::Result<()> {
    write_frame(output, message)
}

fn decode_message<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> io::Result<T> {
    if bytes
        .iter()
        .copied()
        .find(|byte| !byte.is_ascii_whitespace())
        != Some(b'{')
    {
        return Err(invalid("plugin message must be a JSON object"));
    }
    serde_json::from_slice(bytes).map_err(invalid)
}

pub fn read_host_message(input: &mut impl Read) -> io::Result<HostMessage> {
    decode_message(&read_frame(input)?)
}
pub fn read_plugin_message(input: &mut impl Read) -> io::Result<PluginMessage> {
    decode_message(&read_frame(input)?)
}

fn check(context: AgentExecutionContext<'_>) -> Result<(), AgentError> {
    if context.cancellation.is_cancelled() {
        return Err(AgentError::Cancelled);
    }
    if Instant::now() >= context.deadline {
        return Err(AgentError::TimedOut);
    }
    Ok(())
}

fn io_error(error: io::Error, dispatched: bool) -> AgentError {
    if dispatched {
        AgentError::OutcomeUnknown(error.to_string())
    } else if error.kind() == io::ErrorKind::TimedOut {
        AgentError::TimedOut
    } else if error.kind() == io::ErrorKind::Interrupted {
        AgentError::Cancelled
    } else {
        AgentError::Transport(error.to_string())
    }
}

fn retry_read(
    input: &mut impl Read,
    bytes: &mut [u8],
    context: AgentExecutionContext<'_>,
    dispatched: bool,
) -> Result<(), AgentError> {
    let mut offset = 0;
    while offset < bytes.len() {
        check(context).map_err(|error| {
            if dispatched {
                AgentError::OutcomeUnknown(error.to_string())
            } else {
                error
            }
        })?;
        match input.read(&mut bytes[offset..]) {
            Ok(0) => {
                return Err(if dispatched {
                    AgentError::OutcomeUnknown("plugin closed stdout".into())
                } else {
                    AgentError::Transport("plugin closed stdout".into())
                });
            }
            Ok(n) => offset += n,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                std::thread::sleep(POLL_INTERVAL)
            }
            Err(error) => return Err(io_error(error, dispatched)),
        }
    }
    Ok(())
}

fn write_wire(
    output: &mut impl Write,
    message: HostMessage,
    context: AgentExecutionContext<'_>,
) -> Result<(), AgentError> {
    let execute = matches!(message, HostMessage::Execute { .. });
    let bytes = encode_frame(message).map_err(|error| AgentError::Rejected(error.to_string()))?;
    let mut offset = 0;
    while offset < bytes.len() {
        let dispatched = execute && offset > 0;
        check(context).map_err(|error| {
            if dispatched {
                AgentError::OutcomeUnknown(error.to_string())
            } else {
                error
            }
        })?;
        match output.write(&bytes[offset..]) {
            Ok(0) => {
                return Err(if dispatched {
                    AgentError::OutcomeUnknown("plugin stdin closed".into())
                } else {
                    AgentError::Transport("plugin stdin closed".into())
                });
            }
            Ok(n) => offset += n,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                std::thread::sleep(POLL_INTERVAL)
            }
            Err(error) => return Err(io_error(error, dispatched)),
        }
    }
    output.flush().map_err(|error| io_error(error, execute))
}

fn read_wire<T: for<'de> Deserialize<'de>>(
    input: &mut impl Read,
    context: AgentExecutionContext<'_>,
    dispatched: bool,
) -> Result<T, AgentError> {
    let mut length = [0; 4];
    retry_read(input, &mut length, context, dispatched)?;
    let length = frame_length(length).map_err(|error| io_error(error, dispatched))?;
    let mut body = vec![0; length];
    retry_read(input, &mut body, context, dispatched)?;
    decode_message(&body).map_err(|error| io_error(error, dispatched))
}

/// Execute one host-side plugin exchange over unbuffered, nonblocking streams.
/// The caller owns the process lifecycle and checks the revision before dispatch.
pub fn invoke(
    input: &mut impl Read,
    output: &mut impl Write,
    request: AgentRequest,
    context: AgentExecutionContext<'_>,
) -> Result<AgentResult, AgentError> {
    check(context)?;
    write_wire(
        output,
        HostMessage::Hello {
            version: PROTOCOL_VERSION,
        },
        context,
    )?;
    let ready: PluginMessage = read_wire(input, context, false)?;
    let PluginMessage::Ready {
        version,
        capabilities,
    } = ready
    else {
        return Err(AgentError::Transport("plugin handshake failed".into()));
    };
    if version != PROTOCOL_VERSION {
        return Err(AgentError::Rejected(
            "plugin protocol version is not supported".into(),
        ));
    }
    if !capabilities
        .iter()
        .any(|capability| capability == &request.capability)
    {
        return Err(AgentError::Rejected(
            "plugin does not provide the requested capability".into(),
        ));
    }
    write_wire(
        output,
        HostMessage::Execute {
            id: 1,
            capability: request.capability,
            inputs: request.inputs,
        },
        context,
    )?;
    let PluginMessage::Result { id, outputs } = read_wire(input, context, true)? else {
        return Err(AgentError::OutcomeUnknown(
            "plugin returned an unexpected message".into(),
        ));
    };
    if id != 1 {
        return Err(AgentError::OutcomeUnknown(
            "plugin result id did not match the request".into(),
        ));
    }
    check(context).map_err(|error| AgentError::OutcomeUnknown(error.to_string()))?;
    Ok(AgentResult { outputs })
}
