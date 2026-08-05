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

/// Exactly the bootloader's declared usage.
fn is_openboot_usage(usage_page: u16, usage: u16) -> bool {
    usage_page == OB_USAGE_PAGE && usage == OB_USAGE
}

/// `0/0` is what a backend reports when it could not parse the report
/// descriptor. It means "cannot tell", not "matches".
fn usage_unknown(usage_page: u16, usage: u16) -> bool {
    usage_page == 0 && usage == 0
}

/// Narrow a VID:PID match set down to the bootloader's interface.
///
/// Ordered, not permissive: if anything reports the exact usage, ONLY those
/// survive. The `0/0` fallback applies solely when nothing does, so a platform
/// that reports usages excludes an application's keyboard and vendor
/// interfaces properly, and a platform that does not gets an honest
/// multi-match error rather than an arbitrary pick.
///
/// The earlier version treated `0/0` as a match unconditionally, which on a
/// device sharing its application's VID:PID let every interface through.
fn narrow_to_bootloader<T: Copy>(candidates: &[(u16, u16, T)]) -> Vec<T> {
    let exact: Vec<T> = candidates
        .iter()
        .filter(|(up, us, _)| is_openboot_usage(*up, *us))
        .map(|(_, _, v)| *v)
        .collect();
    if !exact.is_empty() {
        return exact;
    }
    candidates
        .iter()
        .filter(|(up, us, _)| usage_unknown(*up, *us))
        .map(|(_, _, v)| *v)
        .collect()
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
        // Serial first, then usage: --serial picks the DEVICE, the usage picks
        // the interface on it. Doing it the other way round would not change
        // the result, but this ordering matches how the two actually divide.
        let by_id: Vec<_> = api
            .device_list()
            .filter(|d| d.vendor_id() == vid && d.product_id() == pid)
            .filter(|d| match serial {
                Some(sn) => d.serial_number() == Some(sn),
                None => true,
            })
            .collect();
        let keyed: Vec<(u16, u16, &_)> = by_id
            .iter()
            .map(|d| (d.usage_page(), d.usage(), d))
            .collect();
        let matches = narrow_to_bootloader(&keyed);

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

        // Ambiguity is an error whether or not a serial was given. A serial
        // narrows to a DEVICE; it cannot choose between interfaces ON one
        // device, because iSerialNumber is a device descriptor field and every
        // interface reports the same string. So surviving paths > 1 after the
        // serial filter means either several devices (serial can help) or one
        // device whose usages were unreadable (nothing can) - and picking the
        // first would silently open, say, a keyboard.
        let paths: BTreeSet<_> = matches.iter().map(|d| d.path()).collect();
        if paths.len() > 1 {
            let serials: Vec<String> = matches
                .iter()
                .map(|d| d.serial_number().unwrap_or("<none>").to_string())
                .collect();
            let hint = if serial.is_some() {
                "same serial on every interface, so --serial cannot choose \
                 between them; this platform does not report HID usages"
            } else {
                "pick one with --serial"
            };
            bail!(
                "{} interfaces match VID=0x{vid:04X} PID=0x{pid:04X}; {hint} \
                 (serials: {})",
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

    /// (usage_page, usage, label) triples, as narrow_to_bootloader takes them.
    const BOOTLOADER: (u16, u16, &str) = (OB_USAGE_PAGE, OB_USAGE, "bootloader");
    const UNKNOWN_A: (u16, u16, &str) = (0, 0, "unknown-a");
    const UNKNOWN_B: (u16, u16, &str) = (0, 0, "unknown-b");
    /// OpenDongle's application interfaces, read off a real device.
    const APP_IFACES: [(u16, u16, &str); 5] = [
        (0x0001, 0x0006, "keyboard"),
        (0x0001, 0x0002, "mouse"),
        (0x000C, 0x0001, "consumer"),
        (0xFF60, 0x0061, "app-vendor"),
        (0xFFFF, 0x0001, "app-vendor2"),
    ];

    #[test]
    fn picks_the_exact_usage_out_of_a_shared_identity_device() {
        let mut set = APP_IFACES.to_vec();
        set.push(BOOTLOADER);

        assert_eq!(narrow_to_bootloader(&set), vec!["bootloader"]);
    }

    #[test]
    fn rejects_every_application_interface_when_no_bootloader_is_present() {
        assert!(narrow_to_bootloader(&APP_IFACES).is_empty());
    }

    #[test]
    fn unknown_usage_is_a_fallback_not_a_match() {
        // With a real usage present, "cannot tell" entries must NOT survive:
        // treating them as matches is what let an application's keyboard
        // through on a shared VID:PID.
        let set = vec![UNKNOWN_A, BOOTLOADER, UNKNOWN_B];
        assert_eq!(narrow_to_bootloader(&set), vec!["bootloader"]);

        // With nothing else to go on, they are all we have.
        let set = vec![UNKNOWN_A, UNKNOWN_B];
        assert_eq!(narrow_to_bootloader(&set), vec!["unknown-a", "unknown-b"]);
    }

    #[test]
    fn a_platform_reporting_no_usages_yields_every_interface_for_the_caller_to_reject() {
        // This is the case the unconditional multi-match error exists for:
        // narrowing cannot help, so open() must refuse rather than guess.
        let blind: Vec<(u16, u16, &str)> = APP_IFACES
            .iter()
            .map(|(_, _, l)| (0u16, 0u16, *l))
            .collect();

        assert_eq!(narrow_to_bootloader(&blind).len(), APP_IFACES.len());
    }

    #[test]
    fn a_partial_usage_match_is_not_enough() {
        let set = vec![
            (OB_USAGE_PAGE, 0x0002, "wrong-usage"),
            (0xFF01, OB_USAGE, "wrong-page"),
        ];
        assert!(narrow_to_bootloader(&set).is_empty());
    }
}
