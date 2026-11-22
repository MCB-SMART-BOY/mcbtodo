//! Bounded, attempt-bound protocol for one-shot local workflow workers.

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

pub const WORKER_PROTOCOL_VERSION: u16 = 2;
pub const MAX_WORKER_FRAME_BYTES: u32 = 8 * 1024 * 1024;
pub const MAX_WORKER_SOURCE_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptIdentity {
    pub run_id: Uuid,
    pub revision: String,
    pub node: String,
    pub attempt_id: Uuid,
    pub epoch: u64,
    pub expected_step: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkerJob {
    Script {
        source: Vec<u8>,
        runner: ScriptRunner,
        timeout_seconds: u32,
        inputs: serde_json::Map<String, Value>,
        executable: Option<Vec<u8>>,
    },
    Agent {
        capability: String,
        timeout_seconds: u32,
        inputs: serde_json::Map<String, Value>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScriptRunner {
    Bash,
    Nushell,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "message", rename_all = "snake_case", deny_unknown_fields)]
pub enum DaemonMessage {
    Prepare {
        identity: AttemptIdentity,
        job: WorkerJob,
    },
    Start {
        identity: AttemptIdentity,
    },
    Cancel {
        identity: AttemptIdentity,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "message", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkerMessage {
    Ready {
        identity: AttemptIdentity,
    },
    Result {
        identity: AttemptIdentity,
        outcome: WorkerOutcome,
        artifact: Option<WorkerArtifact>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkerOutcome {
    Succeeded {
        outputs: serde_json::Map<String, Value>,
    },
    Failed {
        message: String,
    },
    /// The Agent may have executed, but the Worker has no result to commit.
    AgentOutcomeUnknown {
        provider_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerArtifact {
    pub media_type: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Error)]
pub enum WorkerProtocolError {
    #[error("worker IPC I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("worker IPC JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("worker IPC frame has invalid size: {0} bytes")]
    FrameSize(u64),
    #[error("unsupported worker protocol version: {0}")]
    Version(u16),
    #[error("worker script source exceeds {MAX_WORKER_SOURCE_BYTES} bytes")]
    SourceSize,
    #[error("worker message contains an invalid attempt identity")]
    InvalidIdentity,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope<T> {
    protocol_version: u16,
    body: T,
}

// Only the two protocol directions are valid bodies. Validate their typed
// fields directly, without building an intermediate JSON object.
mod body {
    use super::WorkerProtocolError;

    pub trait Message {
        fn validate(&self) -> Result<(), WorkerProtocolError>;
    }
}

pub fn read<T: DeserializeOwned + body::Message>(
    reader: &mut impl Read,
) -> Result<T, WorkerProtocolError> {
    let mut header = [0; 4];
    reader.read_exact(&mut header)?;
    let size = u32::from_be_bytes(header);
    if size == 0 || size > MAX_WORKER_FRAME_BYTES {
        return Err(WorkerProtocolError::FrameSize(size.into()));
    }
    let mut data = vec![0; size as usize];
    reader.read_exact(&mut data)?;
    let envelope: Envelope<T> = serde_json::from_slice(&data)?;
    if envelope.protocol_version != WORKER_PROTOCOL_VERSION {
        return Err(WorkerProtocolError::Version(envelope.protocol_version));
    }
    envelope.body.validate()?;
    Ok(envelope.body)
}

pub fn write<T: Serialize + body::Message>(
    writer: &mut impl Write,
    body: &T,
) -> Result<(), WorkerProtocolError> {
    body.validate()?;
    let envelope = Envelope {
        protocol_version: WORKER_PROTOCOL_VERSION,
        body,
    };
    let mut buffer = LimitedBuffer::new(MAX_WORKER_FRAME_BYTES as usize);
    if let Err(error) = serde_json::to_writer(&mut buffer, &envelope) {
        if error.is_io() {
            return Err(WorkerProtocolError::FrameSize(
                u64::from(MAX_WORKER_FRAME_BYTES) + 1,
            ));
        }
        return Err(error.into());
    }
    writer.write_all(&(buffer.bytes.len() as u32).to_be_bytes())?;
    writer.write_all(&buffer.bytes)?;
    writer.flush()?;
    Ok(())
}

struct LimitedBuffer {
    bytes: Vec<u8>,
    limit: usize,
}

impl LimitedBuffer {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
        }
    }
}

impl Write for LimitedBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(io::Error::other("worker frame size limit exceeded"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl body::Message for DaemonMessage {
    fn validate(&self) -> Result<(), WorkerProtocolError> {
        let identity = match self {
            Self::Prepare { identity, .. }
            | Self::Start { identity }
            | Self::Cancel { identity } => identity,
        };
        identity.validate()?;
        if let Self::Prepare {
            job: WorkerJob::Script {
                source, executable, ..
            },
            ..
        } = self
        {
            if source.len() > MAX_WORKER_SOURCE_BYTES {
                return Err(WorkerProtocolError::SourceSize);
            }
            if let Some(bytes) = executable
                && bytes.len() > 2 * MAX_WORKER_SOURCE_BYTES
            {
                return Err(WorkerProtocolError::FrameSize(bytes.len() as u64));
            }
        }
        Ok(())
    }
}

impl body::Message for WorkerMessage {
    fn validate(&self) -> Result<(), WorkerProtocolError> {
        match self {
            Self::Ready { identity } | Self::Result { identity, .. } => identity.validate(),
        }
    }
}

impl AttemptIdentity {
    fn validate(&self) -> Result<(), WorkerProtocolError> {
        if self.revision.len() != 64
            || !self
                .revision
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || self.node.is_empty()
            || self.node.len() > 256
            || self.epoch == 0
            || self.expected_step == 0
        {
            return Err(WorkerProtocolError::InvalidIdentity);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> AttemptIdentity {
        AttemptIdentity {
            run_id: Uuid::nil(),
            revision: "a".repeat(64),
            node: "test".into(),
            attempt_id: Uuid::from_u128(1),
            epoch: 7,
            expected_step: 3,
        }
    }

    #[test]
    fn round_trips_identity_bound_worker_messages() {
        let message = WorkerMessage::Ready {
            identity: identity(),
        };
        let mut bytes = Vec::new();
        write(&mut bytes, &message).unwrap();
        assert_eq!(
            read::<WorkerMessage>(&mut bytes.as_slice()).unwrap(),
            message
        );
    }

    #[test]
    fn uncertain_agent_outcome_carries_status_and_provider_identity() {
        let outcome = WorkerOutcome::AgentOutcomeUnknown {
            provider_id: "mcbtodo-local-agent".into(),
        };
        assert_eq!(
            serde_json::to_value(&outcome).unwrap(),
            serde_json::json!({
                "status": "agent_outcome_unknown",
                "provider_id": "mcbtodo-local-agent"
            })
        );
        let message = WorkerMessage::Result {
            identity: identity(),
            outcome,
            artifact: None,
        };
        let mut frame = Vec::new();
        write(&mut frame, &message).unwrap();
        assert_eq!(
            read::<WorkerMessage>(&mut frame.as_slice()).unwrap(),
            message
        );
    }

    #[test]
    fn rejects_oversized_frames_before_allocation() {
        let bytes = (MAX_WORKER_FRAME_BYTES + 1).to_be_bytes().to_vec();
        assert!(matches!(
            read::<WorkerMessage>(&mut bytes.as_slice()),
            Err(WorkerProtocolError::FrameSize(_))
        ));
    }

    #[test]
    fn rejects_an_oversized_result_before_writing_a_frame() {
        let message = WorkerMessage::Result {
            identity: identity(),
            outcome: WorkerOutcome::Failed {
                message: "x".repeat(MAX_WORKER_FRAME_BYTES as usize),
            },
            artifact: None,
        };
        let mut bytes = Vec::new();
        assert!(matches!(
            write(&mut bytes, &message),
            Err(WorkerProtocolError::FrameSize(_))
        ));
        assert!(bytes.is_empty());
    }

    #[test]
    fn rejects_unknown_fields() {
        let body = serde_json::to_vec(&serde_json::json!({
            "protocol_version": WORKER_PROTOCOL_VERSION,
            "body": {"message": "ready", "identity": identity(), "extra": true},
        }))
        .unwrap();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(body.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&body);
        assert!(matches!(
            read::<WorkerMessage>(&mut bytes.as_slice()),
            Err(WorkerProtocolError::Json(_))
        ));
    }

    #[test]
    fn rejects_oversized_script_sources() {
        let message = DaemonMessage::Prepare {
            identity: identity(),
            job: WorkerJob::Script {
                source: vec![0; MAX_WORKER_SOURCE_BYTES + 1],
                runner: ScriptRunner::Bash,
                timeout_seconds: 1,
                inputs: Default::default(),
                executable: None,
            },
        };
        assert!(matches!(
            write(&mut Vec::new(), &message),
            Err(WorkerProtocolError::SourceSize)
        ));
    }

    #[test]
    fn rejects_oversized_executable_on_write_and_read() {
        let size = 2 * MAX_WORKER_SOURCE_BYTES + 1;
        let message = DaemonMessage::Prepare {
            identity: identity(),
            job: WorkerJob::Script {
                source: Vec::new(),
                runner: ScriptRunner::Bash,
                timeout_seconds: 1,
                inputs: Default::default(),
                executable: Some(vec![0; size]),
            },
        };
        let mut bytes = Vec::new();
        assert!(matches!(
            write(&mut bytes, &message),
            Err(WorkerProtocolError::FrameSize(actual)) if actual == size as u64
        ));
        assert!(bytes.is_empty());

        let data = serde_json::to_vec(&Envelope {
            protocol_version: WORKER_PROTOCOL_VERSION,
            body: message,
        })
        .unwrap();
        bytes.extend_from_slice(&(data.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&data);
        assert!(matches!(
            read::<DaemonMessage>(&mut bytes.as_slice()),
            Err(WorkerProtocolError::FrameSize(actual)) if actual == size as u64
        ));
    }

    #[test]
    fn rejects_invalid_attempt_identity_on_write_and_read() {
        let mut invalid = identity();
        invalid.revision = "../revision".into();
        let message = WorkerMessage::Ready { identity: invalid };
        assert!(matches!(
            write(&mut Vec::new(), &message),
            Err(WorkerProtocolError::InvalidIdentity)
        ));

        let body = serde_json::to_vec(&Envelope {
            protocol_version: WORKER_PROTOCOL_VERSION,
            body: message,
        })
        .unwrap();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(body.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&body);
        assert!(matches!(
            read::<WorkerMessage>(&mut bytes.as_slice()),
            Err(WorkerProtocolError::InvalidIdentity)
        ));
    }
}
