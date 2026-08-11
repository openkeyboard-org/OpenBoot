//! Selecting one HID interface out of a device, shared by every HID-based
//! transport.
//!
//! VID:PID is not on its own enough. A product may build the bootloader with
//! its application's identity (`OB_USB_VID`/`OB_USB_PID`), so both modes
//! enumerate the same and the application's other HID interfaces — keyboard,
//! mouse, its own vendor interface — sit behind the same VID:PID. The usage
//! page/usage pair is what separates them, and a tunnel on a keyboard has more
//! interfaces to get past than a dongle does.

use std::collections::BTreeSet;

use anyhow::{bail, Context, Result};
use hidapi::{HidApi, HidDevice};

use crate::proto::consts::OB_MAX_FRAME;

/// The usage page/usage pair identifying one interface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsageFilter {
    pub page: u16,
    pub usage: u16,
}

fn matches(filter: UsageFilter, usage_page: u16, usage: u16) -> bool {
    usage_page == filter.page && usage == filter.usage
}

/// `0/0` is what a backend reports when it could not parse the report
/// descriptor. It means "cannot tell", not "matches".
fn usage_unknown(usage_page: u16, usage: u16) -> bool {
    usage_page == 0 && usage == 0
}

/// Narrow a VID:PID match set down to the wanted interface.
///
/// Ordered, not permissive: if anything reports the exact usage, ONLY those
/// survive. The `0/0` fallback applies solely when nothing does, so a platform
/// that reports usages excludes an application's keyboard and vendor
/// interfaces properly, and a platform that does not gets an honest
/// multi-match error rather than an arbitrary pick.
pub fn narrow<T: Copy>(filter: UsageFilter, candidates: &[(u16, u16, T)]) -> Vec<T> {
    let exact: Vec<T> = candidates
        .iter()
        .filter(|(up, us, _)| matches(filter, *up, *us))
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

/// Find and open one interface by VID/PID and usage, optionally filtered by
/// the USB serial number. `what` names the thing being looked for in
/// diagnostics. Stale input reports are drained so the first exchange sees a
/// fresh reply.
pub fn open_hid(
    vid: u16,
    pid: u16,
    serial: Option<&str>,
    filter: UsageFilter,
    what: &str,
) -> Result<(HidDevice, String)> {
    let api = HidApi::new().context("initialize hidapi")?;
    // Serial first, then usage: --serial picks the DEVICE, the usage picks the
    // interface on it. Doing it the other way round would not change the
    // result, but this ordering matches how the two actually divide.
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
    let found = narrow(filter, &keyed);

    let Some(info) = found.first() else {
        let by_serial = match serial {
            Some(sn) => format!(" with serial {sn}"),
            None => String::new(),
        };
        bail!(
            "no {what} for VID=0x{vid:04X} PID=0x{pid:04X}{by_serial} on usage \
             page 0x{:04X} usage 0x{:04X} (on Linux, check hidraw permissions \
             / udev rules — see tools/README.md)",
            filter.page,
            filter.usage
        );
    };

    // Ambiguity is an error whether or not a serial was given. A serial narrows
    // to a DEVICE; it cannot choose between interfaces ON one device, because
    // iSerialNumber is a device descriptor field and every interface reports
    // the same string. So surviving paths > 1 after the serial filter means
    // either several devices (serial can help) or one device whose usages were
    // unreadable (nothing can) — and picking the first would silently open,
    // say, a keyboard.
    let paths: BTreeSet<_> = found.iter().map(|d| d.path()).collect();
    if paths.len() > 1 {
        let serials: Vec<String> = found
            .iter()
            .map(|d| d.serial_number().unwrap_or("<none>").to_string())
            .collect();
        let hint = if serial.is_some() {
            "same serial on every interface, so --serial cannot choose between \
             them; this platform does not report HID usages"
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

    Ok((dev, path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::hid::{OB_USAGE, OB_USAGE_PAGE};

    const BOOT: UsageFilter = UsageFilter {
        page: OB_USAGE_PAGE,
        usage: OB_USAGE,
    };

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

        assert_eq!(narrow(BOOT, &set), vec!["bootloader"]);
    }

    #[test]
    fn rejects_every_application_interface_when_no_bootloader_is_present() {
        assert!(narrow(BOOT, &APP_IFACES).is_empty());
    }

    #[test]
    fn unknown_usage_is_a_fallback_not_a_match() {
        // With a real usage present, "cannot tell" entries must NOT survive:
        // treating them as matches is what let an application's keyboard
        // through on a shared VID:PID.
        let set = vec![UNKNOWN_A, BOOTLOADER, UNKNOWN_B];
        assert_eq!(narrow(BOOT, &set), vec!["bootloader"]);

        // With nothing else to go on, they are all we have.
        let set = vec![UNKNOWN_A, UNKNOWN_B];
        assert_eq!(narrow(BOOT, &set), vec!["unknown-a", "unknown-b"]);
    }

    #[test]
    fn a_platform_reporting_no_usages_yields_every_interface_for_the_caller_to_reject() {
        // This is the case the unconditional multi-match error exists for:
        // narrowing cannot help, so open_hid() must refuse rather than guess.
        let blind: Vec<(u16, u16, &str)> = APP_IFACES
            .iter()
            .map(|(_, _, l)| (0u16, 0u16, *l))
            .collect();

        assert_eq!(narrow(BOOT, &blind).len(), APP_IFACES.len());
    }

    #[test]
    fn a_partial_usage_match_is_not_enough() {
        let set = vec![
            (OB_USAGE_PAGE, 0x0002, "wrong-usage"),
            (0xFF01, OB_USAGE, "wrong-page"),
        ];
        assert!(narrow(BOOT, &set).is_empty());
    }
}
