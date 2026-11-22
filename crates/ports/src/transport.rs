use std::{
    io::{self, Read, Write},
    path::Path,
    time::Instant,
};

use crate::Cancellation;

/// OS-authenticated relationship of the connected peer to the host process.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalPeer {
    SameUser,
    DifferentUser,
}

/// A synchronous byte stream. The protocol caller authorizes the peer and owns
/// framing and message limits.
pub trait LocalConnection: Read + Write {
    fn peer(&self) -> LocalPeer;
}

/// Connects to a workspace-local endpoint using one absolute deadline for
/// connection and subsequent I/O. Implementations must observe cancellation
/// throughout and return an error if peer identity cannot be authenticated.
pub trait LocalTransport: Send + Sync {
    fn connect<'a>(
        &self,
        endpoint: &Path,
        deadline: Instant,
        cancellation: &'a dyn Cancellation,
    ) -> io::Result<Box<dyn LocalConnection + 'a>>;
}
