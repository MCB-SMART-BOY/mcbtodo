//! Length-prefixed JSON frames for the local protocol.

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::value::RawValue;
use thiserror::Error;

use mcbtodo_domain::{InboxSummary, OutlineItem};

use crate::{ErrorCode, Response};

pub const PROTOCOL_VERSION: u16 = 1;
pub const MAX_FRAME_BYTES: u32 = 64 * 1024;

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("IPC I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("IPC JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid IPC frame size: {0} bytes")]
    FrameSize(u64),
    #[error("unsupported IPC protocol version: {0} (expected {PROTOCOL_VERSION})")]
    UnsupportedVersion(u16),
}

#[derive(Deserialize, Serialize)]
struct Envelope<T> {
    protocol_version: u16,
    body: T,
}

pub fn read<T: DeserializeOwned>(reader: &mut impl Read) -> Result<T, ProtocolError> {
    let mut header = [0; 4];
    reader.read_exact(&mut header)?;
    let size = u32::from_be_bytes(header);
    if size == 0 || size > MAX_FRAME_BYTES {
        return Err(ProtocolError::FrameSize(size.into()));
    }
    let mut data = vec![0; size as usize];
    reader.read_exact(&mut data)?;
    let frame: Envelope<&RawValue> = serde_json::from_slice(&data)?;
    if frame.protocol_version != PROTOCOL_VERSION {
        return Err(ProtocolError::UnsupportedVersion(frame.protocol_version));
    }
    Ok(serde_json::from_str(frame.body.get())?)
}

pub fn write<T: Serialize>(writer: &mut impl Write, body: &T) -> Result<(), ProtocolError> {
    let data = serde_json::to_vec(&Envelope {
        protocol_version: PROTOCOL_VERSION,
        body,
    })?;
    let size = data.len() as u64;
    if size == 0 || size > u64::from(MAX_FRAME_BYTES) {
        return Err(ProtocolError::FrameSize(size));
    }
    writer.write_all(&(size as u32).to_be_bytes())?;
    writer.write_all(&data)?;
    writer.flush()?;
    Ok(())
}

pub fn write_outline(
    writer: &mut impl Write,
    items: Vec<OutlineItem>,
) -> Result<(), ProtocolError> {
    write_rows(writer, items, |items, more| Response::Outline {
        items,
        more,
    })
}

pub fn write_inbox(writer: &mut impl Write, items: Vec<InboxSummary>) -> Result<(), ProtocolError> {
    write_rows(writer, items, |items, more| Response::Inbox { items, more })
}

/// Write one complete result as ordered frames. The reader only publishes
/// the rows after receiving the final frame.
fn write_rows<T: Serialize>(
    writer: &mut impl Write,
    items: Vec<T>,
    make_response: impl Fn(Vec<T>, bool) -> Response,
) -> Result<(), ProtocolError> {
    // Leave room for the fixed envelope fields and array separators.
    const ROW_BUDGET: usize = MAX_FRAME_BYTES as usize - 128;
    let mut bytes = 0;
    let mut frame = Vec::new();
    for item in items {
        let size = serde_json::to_vec(&item)?.len() + 1;
        if size > ROW_BUDGET {
            return write(
                writer,
                &Response::Error {
                    code: ErrorCode::InvalidRequest,
                    message: "a result row exceeds the IPC size limit".into(),
                },
            );
        }
        if bytes + size > ROW_BUDGET {
            write(writer, &make_response(std::mem::take(&mut frame), true))?;
            bytes = 0;
        }
        frame.push(item);
        bytes += size;
    }
    write(writer, &make_response(frame, false))
}
