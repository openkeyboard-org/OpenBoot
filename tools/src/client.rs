//! Typed command layer over any `Transport`.
//!
//! `BootClient` exposes one method per OBP command, each of which owns its
//! payload encoding, its timeout, and — declared in the method, not at the
//! call site — its [`RetryPolicy`]. The retry rule lives HERE rather than
//! in the transport: a resend happens only on a timeout or on a `0xFF`
//! frame-error report (the device saw a corrupt frame), each attempt with
//! a fresh `seq`. A decoded status error is a definitive device answer and
//! is never retried. All v0.2 mutating commands are idempotent by design
//! (addressed WRITE + erased-block bitmap; COMMIT re-attests the same
//! record) — except BOOT, which is `RetryPolicy::Once`: a successful BOOT
//! tears the session down, so a lost response is indistinguishable from
//! success and a blind resend could land on the freshly booted app.

use std::time::Duration;

use crate::proto::consts::{
    OB_BOOT_APP, OB_BOOT_STAY, OB_CMD_BOOT, OB_CMD_COMMIT, OB_CMD_CRC, OB_CMD_ERASE, OB_CMD_HELLO,
    OB_CMD_WRITE, OB_PROTO_MAJOR, OB_PROTO_MINOR,
};
use crate::proto::device_info::DeviceInfo;
use crate::proto::{self, DeviceError, Frame};
use crate::transport::{Transport, TransportError};

pub const MAX_ATTEMPTS: usize = 3;

pub const HELLO_TIMEOUT: Duration = Duration::from_millis(500);
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(1);
pub const CRC_TIMEOUT: Duration = Duration::from_secs(3);
pub const COMMIT_TIMEOUT: Duration = Duration::from_secs(3);
pub const BOOT_TIMEOUT: Duration = Duration::from_secs(1);

/// Per-ERASE-request timeout: fixed cost + per-block erase time.
pub fn erase_timeout(blocks: u32) -> Duration {
    Duration::from_millis(200 + 30 * u64::from(blocks))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetryPolicy {
    /// Make at most `n` attempts in total (so `n - 1` resends) on timeout
    /// or frame-error. A device-reported status is never a retry reason.
    Retry(usize),
    /// Send exactly once; a lost response is fatal (BOOT).
    Once,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootMode {
    App,
    Stay,
}

impl BootMode {
    fn wire(self) -> u8 {
        match self {
            BootMode::App => OB_BOOT_APP,
            BootMode::Stay => OB_BOOT_STAY,
        }
    }
}

/// Why an attempt was retryable. Kept separate from `ClientError` so the
/// exhausted-retries message can carry the last cause as a `source`.
#[derive(Debug, thiserror::Error)]
pub enum RetryReason {
    #[error(transparent)]
    Timeout(#[from] TransportError),
    #[error("device reported a frame error: {0}")]
    FrameError(String),
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("{name}: no valid response after {attempts} attempts")]
    Exhausted {
        name: &'static str,
        attempts: usize,
        #[source]
        last: RetryReason,
    },
    /// A `RetryPolicy::Once` command whose single attempt did not land.
    #[error("{name} failed")]
    NoResponse {
        name: &'static str,
        #[source]
        reason: RetryReason,
    },
    /// The device answered with a non-OK status: definitive, never retried.
    #[error("{name} failed")]
    Device {
        name: &'static str,
        #[source]
        source: DeviceError,
    },
    #[error("{name}: malformed response")]
    BadResponse {
        name: &'static str,
        #[source]
        source: anyhow::Error,
    },
    #[error("{name}: transport error")]
    Io {
        name: &'static str,
        #[source]
        source: anyhow::Error,
    },
    #[error("device speaks OBP {major}.{minor}; this tool supports major version {supported}")]
    ProtoVersion { major: u8, minor: u8, supported: u8 },
}

enum Attempt {
    Done(Vec<u8>),
    Retry(RetryReason),
}

/// One bootloader session: owns the `seq` counter and the retry loop.
pub struct BootClient<'a> {
    transport: &'a mut dyn Transport,
    seq: u8,
}

impl<'a> BootClient<'a> {
    pub fn new(transport: &'a mut dyn Transport) -> BootClient<'a> {
        BootClient { transport, seq: 0 }
    }

    /// See `Transport::holds_in_bootloader`.
    pub fn holds_in_bootloader(&self) -> bool {
        self.transport.holds_in_bootloader()
    }

    fn next_seq(&mut self) -> u8 {
        let seq = self.seq;
        self.seq = self.seq.wrapping_add(1);
        seq
    }

    /// One send + one matched response. `Done` carries the OK payload
    /// (status byte included); `Retry` carries a timeout or frame-error
    /// report; a hard error (IO or a non-OK device status) returns `Err`.
    fn attempt(
        &mut self,
        name: &'static str,
        cmd: u8,
        payload: &[u8],
        timeout: Duration,
    ) -> Result<Attempt, ClientError> {
        let req = Frame::new(cmd, self.next_seq(), payload.to_vec());
        match self.transport.xfer(&req, timeout) {
            Ok(resp) if resp.is_frame_error() => {
                let status = resp.payload.first().copied().unwrap_or(0);
                let detail = resp.payload.get(1).copied();
                Ok(Attempt::Retry(RetryReason::FrameError(
                    proto::describe_status(status, detail),
                )))
            }
            Ok(resp) => {
                if resp.payload.is_empty() {
                    return Err(ClientError::BadResponse {
                        name,
                        source: anyhow::anyhow!("response carried no status byte"),
                    });
                }
                match DeviceError::from_payload(&resp.payload) {
                    Some(err) => Err(ClientError::Device { name, source: err }),
                    None => Ok(Attempt::Done(resp.payload)),
                }
            }
            // A timeout is retryable; anything else the transport reports
            // is fatal. Typed match, no downcast.
            Err(TransportError::Timeout) => Ok(Attempt::Retry(RetryReason::Timeout(
                TransportError::Timeout,
            ))),
            Err(TransportError::Io(source)) => Err(ClientError::Io { name, source }),
        }
    }

    fn run(
        &mut self,
        name: &'static str,
        cmd: u8,
        payload: &[u8],
        timeout: Duration,
        policy: RetryPolicy,
    ) -> Result<Vec<u8>, ClientError> {
        let attempts = match policy {
            RetryPolicy::Retry(n) => n,
            RetryPolicy::Once => 1,
        };
        let mut last: Option<RetryReason> = None;
        for _ in 0..attempts {
            match self.attempt(name, cmd, payload, timeout)? {
                Attempt::Done(resp) => return Ok(resp),
                Attempt::Retry(reason) => last = Some(reason),
            }
        }
        let last = last.expect("at least one attempt is always made");
        match policy {
            RetryPolicy::Once => Err(ClientError::NoResponse { name, reason: last }),
            RetryPolicy::Retry(_) => Err(ClientError::Exhausted {
                name,
                attempts,
                last,
            }),
        }
    }

    /// HELLO handshake; also (re)opens the device session, clearing the
    /// erased-block bitmap and the stream CRC on the device side.
    pub fn hello(&mut self) -> Result<DeviceInfo, ClientError> {
        let payload = proto::hello_req_payload(OB_PROTO_MAJOR, OB_PROTO_MINOR);
        let resp = self.run(
            "HELLO",
            OB_CMD_HELLO,
            &payload,
            HELLO_TIMEOUT,
            RetryPolicy::Retry(MAX_ATTEMPTS),
        )?;
        let info = DeviceInfo::parse(&resp).map_err(|source| ClientError::BadResponse {
            name: "HELLO",
            source,
        })?;
        if info.proto_major != OB_PROTO_MAJOR {
            return Err(ClientError::ProtoVersion {
                major: info.proto_major,
                minor: info.proto_minor,
                supported: OB_PROTO_MAJOR,
            });
        }
        Ok(info)
    }

    pub fn erase(&mut self, addr: u32, len: u32, blocks: u32) -> Result<(), ClientError> {
        self.run(
            "ERASE",
            OB_CMD_ERASE,
            &proto::erase_req_payload(addr, len),
            erase_timeout(blocks),
            RetryPolicy::Retry(MAX_ATTEMPTS),
        )
        .map(|_| ())
    }

    pub fn write(&mut self, addr: u32, data: &[u8]) -> Result<(), ClientError> {
        self.run(
            "WRITE",
            OB_CMD_WRITE,
            &proto::write_req_payload(addr, data),
            WRITE_TIMEOUT,
            RetryPolicy::Retry(MAX_ATTEMPTS),
        )
        .map(|_| ())
    }

    pub fn commit(&mut self, img_len: u32, img_crc32: u32) -> Result<(), ClientError> {
        self.run(
            "COMMIT",
            OB_CMD_COMMIT,
            &proto::commit_req_payload(img_len, img_crc32),
            COMMIT_TIMEOUT,
            RetryPolicy::Retry(MAX_ATTEMPTS),
        )
        .map(|_| ())
    }

    pub fn crc(&mut self, addr: u32, len: u32) -> Result<u32, ClientError> {
        let resp = self.run(
            "CRC",
            OB_CMD_CRC,
            &proto::crc_req_payload(addr, len),
            CRC_TIMEOUT,
            RetryPolicy::Retry(MAX_ATTEMPTS),
        )?;
        proto::crc_resp_value(&resp).map_err(|source| ClientError::BadResponse {
            name: "CRC",
            source,
        })
    }

    /// BOOT is the one command that is never resent (see the module note).
    pub fn boot(&mut self, mode: BootMode) -> Result<(), ClientError> {
        self.run(
            "BOOT",
            OB_CMD_BOOT,
            &proto::boot_req_payload(mode.wire()),
            BOOT_TIMEOUT,
            RetryPolicy::Once,
        )
        .map(|_| ())
    }
}

#[cfg(test)]
mod tests;
