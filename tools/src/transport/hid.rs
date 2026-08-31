//! USB HID transport: one frame per 64-byte interrupt report, zero-padded,
//! both directions. Writes carry hidapi's leading 0x00 report-ID byte (the
//! device uses no report IDs); reads come back as the bare 64-byte report.
//!
//! Why HID: it rides each OS's native HID stack (hidraw on Linux, hid.dll
//! on Windows, IOKit on macOS), so no driver installation anywhere.

use std::time::Instant;

use anyhow::{Context, Result};
use hidapi::HidDevice;

use super::hidsel::{self, UsageFilter};
use super::Transport;
use crate::proto::consts::OB_MAX_FRAME;

/// pid.codes test allocation used during development.
pub const DEFAULT_VID: u16 = 0x1209;
pub const DEFAULT_PID: u16 = 0x0001;

/// The bootloader's HID report descriptor: vendor usage page 0xFF00, usage
/// 0x01 (see PROTOCOL.md section 12).
///
/// Normative, and deliberately not overridable from the command line: pointing
/// this selector at some other interface is how you write flash frames into a
/// keyboard.
pub const OB_USAGE_PAGE: u16 = 0xFF00;
pub const OB_USAGE: u16 = 0x0001;

pub struct HidTransport {
    dev: HidDevice,
    /// Human-readable device path for logging.
    pub path: String,
}

impl HidTransport {
    /// Find and open the bootloader by VID/PID, optionally filtered by the
    /// USB serial number (the device's 16-hex ROM UID). Stale input reports
    /// are drained so the first xfer sees a fresh reply.
    pub fn open(vid: u16, pid: u16, serial: Option<&str>) -> Result<HidTransport> {
        let filter = UsageFilter {
            page: OB_USAGE_PAGE,
            usage: OB_USAGE,
        };
        let (dev, path) = hidsel::open_hid(vid, pid, serial, filter, "bootloader")
            .context("is the device in the bootloader?")?;
        Ok(HidTransport { dev, path })
    }
}

impl Transport for HidTransport {
    fn send_frame(&mut self, frame: &[u8]) -> Result<()> {
        // 0x00 report-ID prefix + frame zero-padded to one 64-byte report.
        let mut report = [0u8; 1 + OB_MAX_FRAME];
        report[1..1 + frame.len()].copy_from_slice(frame);
        self.dev.write(&report).context("HID write")?;
        Ok(())
    }

    fn recv_frame(&mut self, deadline: Instant) -> Result<Option<Vec<u8>>> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(None);
        }
        let ms = i32::try_from(remaining.as_millis())
            .unwrap_or(i32::MAX)
            .max(1);
        let mut buf = [0u8; OB_MAX_FRAME];
        let n = self.dev.read_timeout(&mut buf, ms).context("HID read")?;
        if n == 0 {
            return Ok(None);
        }
        Ok(Some(buf[..n].to_vec()))
    }
}
