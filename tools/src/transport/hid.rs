//! USB HID transport: one frame per 64-byte interrupt report, zero-padded,
//! both directions. Writes carry hidapi's leading 0x00 report-ID byte (the
//! device uses no report IDs); reads come back as the bare 64-byte report.
//!
//! Why HID: it rides each OS's native HID stack (hidraw on Linux, hid.dll
//! on Windows, IOKit on macOS), so no driver installation anywhere.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use hidapi::{HidApi, HidDevice};

use super::{xfer_link, FrameLink, Transport, TransportError};
use crate::proto::consts::OB_MAX_FRAME;
use crate::proto::Frame;

/// pid.codes test allocation used during development.
pub const DEFAULT_VID: u16 = 0x1209;
pub const DEFAULT_PID: u16 = 0x0001;

/// The bootloader's HID report descriptor: vendor usage page 0xFF00, usage
/// 0x01 (see PROTOCOL.md section 12).
///
/// VID:PID is not on its own enough to find the bootloader. A product may
/// build it with its application's identity (`OB_USB_VID`/`OB_USB_PID`), so
/// both modes enumerate the same and the application's other HID interfaces
/// — keyboard, mouse, its own vendor interface — sit behind the same
/// VID:PID. This pair is what separates them.
pub const OB_USAGE_PAGE: u16 = 0xFF00;
pub const OB_USAGE: u16 = 0x0001;

/// Whether an enumerated interface looks like OpenBoot's.
///
/// Platforms that do not report a usage pass `0/0`; treat that as "cannot
/// tell" and keep the entry rather than discarding the only candidate. On a
/// device whose bootloader has its own VID:PID this changes nothing, and on
/// a shared-identity device the caller still gets a clear multi-match error
/// instead of silently opening a keyboard interface.
fn looks_like_openboot(usage_page: u16, usage: u16) -> bool {
    (usage_page == 0 && usage == 0) || (usage_page == OB_USAGE_PAGE && usage == OB_USAGE)
}

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
        let api = HidApi::new().context("initialize hidapi")?;
        let matches: Vec<_> = api
            .device_list()
            .filter(|d| d.vendor_id() == vid && d.product_id() == pid)
            .filter(|d| looks_like_openboot(d.usage_page(), d.usage()))
            .filter(|d| match serial {
                Some(sn) => d.serial_number() == Some(sn),
                None => true,
            })
            .collect();

        let Some(info) = matches.first() else {
            let filter = match serial {
                Some(sn) => format!(" with serial {sn}"),
                None => String::new(),
            };
            bail!(
                "no HID device for VID=0x{vid:04X} PID=0x{pid:04X}{filter} \
                 on usage page 0x{OB_USAGE_PAGE:04X} usage 0x{OB_USAGE:04X} \
                 (is the device in the bootloader? on Linux, check hidraw \
                 permissions / udev rules — see tools/README.md)"
            );
        };

        // One HID interface per bootloader, so each physical device is one
        // surviving enumeration entry; multiple distinct paths mean multiple
        // devices and the caller must disambiguate by serial.
        let paths: BTreeSet<_> = matches.iter().map(|d| d.path()).collect();
        if paths.len() > 1 && serial.is_none() {
            let serials: Vec<String> = matches
                .iter()
                .map(|d| d.serial_number().unwrap_or("<none>").to_string())
                .collect();
            bail!(
                "{} devices match VID=0x{vid:04X} PID=0x{pid:04X}; pick one \
                 with --serial (found: {})",
                paths.len(),
                serials.join(", ")
            );
        }

        let path = info.path().to_string_lossy().into_owned();
        let dev = info
            .open_device(&api)
            .with_context(|| format!("open HID device {path}"))?;

        // Drain any buffered input reports left over from an earlier session.
        let _ = dev.set_blocking_mode(false);
        let mut buf = [0u8; OB_MAX_FRAME];
        for _ in 0..64 {
            match dev.read_timeout(&mut buf, 0) {
                Ok(0) | Err(_) => break,
                Ok(_) => continue,
            }
        }
        let _ = dev.set_blocking_mode(true);

        Ok(HidTransport { dev, path })
    }
}

impl FrameLink for HidTransport {
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

impl Transport for HidTransport {
    fn xfer(&mut self, req: &Frame, timeout: Duration) -> Result<Frame, TransportError> {
        xfer_link(self, req, timeout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_the_bootloader_usage() {
        assert!(looks_like_openboot(OB_USAGE_PAGE, OB_USAGE));
    }

    #[test]
    fn rejects_other_interfaces_behind_the_same_vid_pid() {
        // A product may build its bootloader with the application's VID:PID,
        // putting the application's interfaces in the same match set. These
        // are OpenDongle's: keyboard, mouse/consumer, and its own two vendor
        // pages. None may be mistaken for the bootloader.
        for (page, usage) in [
            (0x0001, 0x0006), // Generic Desktop / Keyboard
            (0x0001, 0x0002), // Generic Desktop / Mouse
            (0x000C, 0x0001), // Consumer Control
            (0xFFFF, 0x0001), // application vendor page
            (0xFF60, 0x0061), // application vendor page
        ] {
            assert!(
                !looks_like_openboot(page, usage),
                "0x{page:04X}/0x{usage:04X} must not match the bootloader"
            );
        }
    }

    #[test]
    fn keeps_entries_from_platforms_that_report_no_usage() {
        // hidapi reports 0/0 where a backend cannot parse the report
        // descriptor. Filtering those out would discard the only candidate
        // and make the tool unusable there, so "cannot tell" must pass.
        assert!(looks_like_openboot(0, 0));
    }

    #[test]
    fn a_partial_usage_match_is_not_enough() {
        assert!(!looks_like_openboot(OB_USAGE_PAGE, 0x0002));
        assert!(!looks_like_openboot(0xFF01, OB_USAGE));
    }
}
