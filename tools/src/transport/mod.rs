//! Transport seam. One `Transport::xfer` call = exactly one request frame
//! sent, then the matched response frame (or the device's 0xFF frame-error
//! report) returned.
//!
//! Retry policy deliberately does NOT live here: it is implemented one layer
//! up, in `client::BootClient`, where each command declares its own
//! `RetryPolicy` — idempotent commands resend with a fresh `seq`, BOOT is
//! `Once` and never resent. What does live
//! here — shared by both transports via `FrameLink` — is response matching:
//! frames with a stale `seq` (answers to a timed-out earlier attempt),
//! frames for other commands, and undecodable/corrupt frames are all
//! discarded, and reading continues until the deadline.

pub mod hid;
pub mod uart;

use std::time::{Duration, Instant};

use anyhow::Result;

use crate::proto::Frame;

/// A bootloader connection: send one request, get the matching response.
pub trait Transport {
    fn xfer(&mut self, req: &Frame, timeout: Duration) -> Result<Frame, TransportError>;
}

/// What a transport exchange can fail with. `Timeout` is a distinct variant
/// rather than a marker inside `anyhow::Error` so the retry decision is a
/// `match`, not a downcast.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("no response before the deadline")]
    Timeout,
    /// Anything the underlying HID/serial stack reported.
    #[error(transparent)]
    Io(#[from] anyhow::Error),
}

/// Byte-level frame link each concrete transport implements. `recv_frame`
/// blocks until one raw candidate frame is available or `deadline` passes
/// (`Ok(None)`); it performs no protocol validation beyond transport
/// framing (HID report boundaries / UART SOF hunt).
pub(crate) trait FrameLink {
    fn send_frame(&mut self, frame: &[u8]) -> Result<()>;
    fn recv_frame(&mut self, deadline: Instant) -> Result<Option<Vec<u8>>>;
}

/// Shared request/response matching over any `FrameLink`.
pub(crate) fn xfer_link(
    link: &mut dyn FrameLink,
    req: &Frame,
    timeout: Duration,
) -> Result<Frame, TransportError> {
    let deadline = Instant::now() + timeout;
    link.send_frame(&req.encode())?;
    while let Some(raw) = link.recv_frame(deadline)? {
        match Frame::decode(&raw) {
            // The device could not parse a request; its echoed seq cannot be
            // trusted, so accept the report regardless of seq.
            Ok(frame) if frame.is_frame_error() => return Ok(frame),
            Ok(frame) if frame.is_response_to(req) => return Ok(frame),
            Ok(_stale) => continue,    // stale seq or foreign cmd: discard
            Err(_corrupt) => continue, // undecodable: discard, keep listening
        }
    }
    Err(TransportError::Timeout)
}
