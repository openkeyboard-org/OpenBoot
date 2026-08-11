//! Transport seam. One `Transport::xfer` call = exactly one request frame
//! sent, then the matched response frame (or the device's 0xFF frame-error
//! report) returned.
//!
//! Retry policy deliberately does NOT live here: it is implemented one layer
//! up, in `client::BootClient`, where each command declares its own
//! `RetryPolicy` — idempotent commands resend with a fresh `seq`, BOOT is
//! `Once` and never resent. What does live
//! here — shared by both transports through the trait's default method — is
//! response matching:
//! frames with a stale `seq` (answers to a timed-out earlier attempt),
//! frames for other commands, and undecodable/corrupt frames are all
//! discarded, and reading continues until the deadline.

pub mod framing;
pub mod hid;
pub mod hidsel;
pub mod qmk;
pub mod uart;

use std::time::{Duration, Instant};

use anyhow::Result;

use crate::proto::Frame;

/// A bootloader connection: send one request, get the matching response.
///
/// `recv_frame` must block until one raw candidate frame is available or
/// `deadline` passes (then `Ok(None)`), performing no protocol validation
/// beyond transport framing (HID report boundaries / UART SOF hunt). The
/// default `xfer` leans on that contract: it treats the first `Ok(None)` as
/// the deadline having expired, so a non-blocking poll would turn every
/// quiet moment into a spurious `Timeout`.
pub trait Transport {
    fn send_frame(&mut self, frame: &[u8]) -> Result<()>;
    fn recv_frame(&mut self, deadline: Instant) -> Result<Option<Vec<u8>>>;

    fn xfer(&mut self, req: &Frame, timeout: Duration) -> Result<Frame, TransportError> {
        let deadline = Instant::now() + timeout;
        self.send_frame(&req.encode())?;
        while let Some(raw) = self.recv_frame(deadline)? {
            match Frame::decode(&raw) {
                // The device could not parse a request; its echoed seq cannot
                // be trusted, so accept the report regardless of seq.
                Ok(frame) if frame.is_frame_error() => return Ok(frame),
                Ok(frame) if frame.is_response_to(req) => return Ok(frame),
                Ok(_stale) => continue, // stale seq or foreign cmd: discard
                Err(_corrupt) => continue, // undecodable: discard, keep listening
            }
        }
        Err(TransportError::Timeout)
    }
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
