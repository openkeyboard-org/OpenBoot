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

/// One HID interface a selector matched, as enumeration reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HidCandidate {
    /// The platform's path for the interface. Good for opening it now; NOT an
    /// identity — a device that re-enumerates may come back under another
    /// path, and another device may take this one.
    pub path: String,
    pub serial: Option<String>,
    pub product: Option<String>,
    /// -1 where the platform does not report one.
    pub interface_number: i32,
    pub usage_page: u16,
    pub usage: u16,
}

/// The interfaces [`open_hid`] would choose between for this VID/PID and
/// usage, without opening any of them. Each path appears once.
///
/// The same narrowing applies: if anything reports the exact usage only those
/// are returned, and interfaces whose usage the platform could not read
/// (`0/0`) are returned only when nothing does. A caller that gets several of
/// the latter cannot tell them apart and must not guess.
pub fn list_hid(vid: u16, pid: u16, filter: UsageFilter) -> Result<Vec<HidCandidate>> {
    let api = HidApi::new().context("initialize hidapi")?;
    let by_id: Vec<_> = api
        .device_list()
        .filter(|d| d.vendor_id() == vid && d.product_id() == pid)
        .collect();
    let keyed: Vec<(u16, u16, &_)> = by_id
        .iter()
        .map(|d| (d.usage_page(), d.usage(), d))
        .collect();
    let mut seen = BTreeSet::new();
    Ok(narrow(filter, &keyed)
        .into_iter()
        .filter(|d| seen.insert(d.path()))
        .map(|d| HidCandidate {
            path: d.path().to_string_lossy().into_owned(),
            serial: d.serial_number().map(str::to_string),
            product: d.product_string().map(str::to_string),
            interface_number: d.interface_number(),
            usage_page: d.usage_page(),
            usage: d.usage(),
        })
        .collect())
}

/// Whether `path` may be opened as the interface `filter` describes, given
/// every interface on its VID:PID as `(usage page, usage, path)`.
///
/// Naming a path does not excuse it from the usage check — that check is what
/// keeps flash frames out of a keyboard interface. So the path must survive
/// the same narrowing as discovery, and where the platform could not read the
/// usages it is accepted only if it is the sole candidate: with several
/// unreadable interfaces nothing distinguishes the right one from the rest.
fn check_path(filter: UsageFilter, candidates: &[(u16, u16, &str)], path: &str) -> Result<()> {
    let keyed: Vec<(u16, u16, (u16, u16, &str))> =
        candidates.iter().map(|c| (c.0, c.1, *c)).collect();
    let mut seen = BTreeSet::new();
    let mut found = narrow(filter, &keyed);
    found.retain(|c| seen.insert(c.2));
    let Some(&(page, usage, _)) = found.iter().find(|c| c.2 == path) else {
        bail!(
            "{path} is not an interface on usage page 0x{:04X} usage 0x{:04X} \
             (it may have re-enumerated under another path)",
            filter.page,
            filter.usage
        );
    };
    if usage_unknown(page, usage) && found.len() > 1 {
        bail!(
            "{path} cannot be told apart from {} other interface(s): this \
             platform does not report HID usages",
            found.len() - 1
        );
    }
    Ok(())
}

/// Open exactly `path`, provided enumeration shows it — now — as the
/// interface `filter` describes on this VID/PID. `what` names the thing being
/// opened in diagnostics.
///
/// This selects an interface, not a physical device. A path observed earlier
/// can be reused by something else after a re-enumeration, so a caller that
/// needs a particular unit must still confirm its identity (the UID in HELLO)
/// before changing anything on it.
pub fn open_hid_path(
    vid: u16,
    pid: u16,
    filter: UsageFilter,
    path: &str,
    what: &str,
) -> Result<(HidDevice, String)> {
    let api = HidApi::new().context("initialize hidapi")?;
    let by_id: Vec<_> = api
        .device_list()
        .filter(|d| d.vendor_id() == vid && d.product_id() == pid)
        .collect();
    let paths: Vec<String> = by_id
        .iter()
        .map(|d| d.path().to_string_lossy().into_owned())
        .collect();
    let candidates: Vec<(u16, u16, &str)> = by_id
        .iter()
        .zip(&paths)
        .map(|(d, p)| (d.usage_page(), d.usage(), p.as_str()))
        .collect();
    check_path(filter, &candidates, path)
        .with_context(|| format!("no {what} at that path for VID=0x{vid:04X} PID=0x{pid:04X}"))?;
    let info = by_id
        .iter()
        .zip(&paths)
        .find(|(_, p)| p.as_str() == path)
        .map(|(d, _)| *d)
        .expect("check_path accepted a path that is in the list");
    open_and_drain(&api, info)
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

    open_and_drain(&api, info)
}

fn open_and_drain(api: &HidApi, info: &hidapi::DeviceInfo) -> Result<(HidDevice, String)> {
    let path = info.path().to_string_lossy().into_owned();
    let dev = info
        .open_device(api)
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
    use crate::transport::qmk::{TUNNEL_USAGE, TUNNEL_USAGE_PAGE};

    const BOOT: UsageFilter = UsageFilter {
        page: OB_USAGE_PAGE,
        usage: OB_USAGE,
    };
    const TUNNEL: UsageFilter = UsageFilter {
        page: TUNNEL_USAGE_PAGE,
        usage: TUNNEL_USAGE,
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

    /// The MK65MX keyboard's full interface set, tunnel included. VIA's
    /// 0xFF60/0x61 sits one nibble from the tunnel's page, so this is the
    /// fixture that keeps the two from ever cross-selecting.
    const KEYBOARD_IFACES: [(u16, u16, &str); 5] = [
        (0x0001, 0x0006, "keyboard"),
        (0x0001, 0x0002, "mouse"),
        (0x000C, 0x0001, "consumer"),
        (0xFF60, 0x0061, "via"),
        (TUNNEL_USAGE_PAGE, TUNNEL_USAGE, "tunnel"),
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

    /// The filters must partition a real keyboard: the tunnel selector must
    /// never land on VIA, and the bootloader selector must never land on the
    /// tunnel.
    #[test]
    fn tunnel_and_bootloader_filters_never_cross_select() {
        assert_eq!(narrow(TUNNEL, &KEYBOARD_IFACES), vec!["tunnel"]);
        assert!(narrow(BOOT, &KEYBOARD_IFACES).is_empty());

        let mut with_bootloader = KEYBOARD_IFACES.to_vec();
        with_bootloader.push(BOOTLOADER);
        assert_eq!(narrow(BOOT, &with_bootloader), vec!["bootloader"]);
        assert_eq!(narrow(TUNNEL, &with_bootloader), vec!["tunnel"]);
    }

    /* --- opening a named path ------------------------------------------- */

    #[test]
    fn a_named_path_must_be_the_interface_the_filter_describes() {
        let mut set = APP_IFACES.to_vec();
        set.push(BOOTLOADER);

        assert!(check_path(BOOT, &set, "bootloader").is_ok());
        // Naming the keyboard does not make it a bootloader.
        let err = check_path(BOOT, &set, "keyboard").unwrap_err();
        assert!(format!("{err:#}").contains("not an interface"), "{err:#}");
        // Nor does naming something that is not there at all.
        assert!(check_path(BOOT, &set, "gone").is_err());
    }

    #[test]
    fn a_named_path_is_refused_when_no_bootloader_is_present() {
        // The application is running: nothing on this VID:PID is OpenBoot, so
        // no path among its interfaces may be opened as one.
        for (_, _, path) in APP_IFACES {
            assert!(check_path(BOOT, &APP_IFACES, path).is_err(), "{path}");
        }
    }

    #[test]
    fn a_named_path_with_an_unreadable_usage_must_be_the_only_candidate() {
        // One interface the platform could not describe: there is nothing to
        // confuse it with, so the caller's choice stands.
        assert!(check_path(BOOT, &[UNKNOWN_A], "unknown-a").is_ok());

        // Several: the path names one, but nothing says it is the right one.
        let err = check_path(BOOT, &[UNKNOWN_A, UNKNOWN_B], "unknown-a").unwrap_err();
        assert!(format!("{err:#}").contains("told apart"), "{err:#}");

        // A real usage elsewhere excludes the unreadable ones entirely.
        let set = vec![UNKNOWN_A, BOOTLOADER];
        assert!(check_path(BOOT, &set, "unknown-a").is_err());
        assert!(check_path(BOOT, &set, "bootloader").is_ok());
    }

    #[test]
    fn a_path_listed_twice_is_one_candidate() {
        // Some backends report an interface once per top-level collection.
        let set = vec![UNKNOWN_A, UNKNOWN_A];
        assert!(check_path(BOOT, &set, "unknown-a").is_ok());
    }

    /// The tunnel's page is deliberately not VIA's, and deliberately not the
    /// bootloader's. Pin that, since both neighbours are one edit away.
    #[test]
    fn the_tunnel_usage_is_distinct_from_its_neighbours() {
        assert_ne!((TUNNEL.page, TUNNEL.usage), (BOOT.page, BOOT.usage));
        assert_ne!((TUNNEL.page, TUNNEL.usage), (0xFF60, 0x0061));
    }
}
