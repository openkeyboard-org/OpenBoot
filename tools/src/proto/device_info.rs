//! HELLO response parsing: the device-info payload that makes the host
//! chip-database-free. Longer payloads are tolerated per the spec ("device
//! must send >= this; host must tolerate more").

use anyhow::{bail, Result};

use super::consts::{
    OB_FAMILY_CH570, OB_FAMILY_CH572, OB_FAMILY_CH591, OB_FAMILY_CH592, OB_FEAT_CRC_LIVE,
    OB_FEAT_READ, OB_HELLO_RESP_LEN, OB_MAX_WRITE_DATA, OB_SLOT_ID_NONE, OB_TRANSPORT_ID_UART,
    OB_TRANSPORT_ID_USB,
};

/// Parsed HELLO device info (offset 0 is the status byte, checked by the
/// caller before parsing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    pub proto_major: u8,
    pub proto_minor: u8,
    pub chip_rev: u8,
    pub bl_version: u16,
    pub chip_family: u8,
    pub transport: u8,
    pub app_start: u32,
    pub app_end: u32,
    pub erase_block: u32,
    pub write_page: u16,
    pub write_align: u8,
    pub max_write_data: u8,
    pub features: u32,
    pub uid: u64,
    /// How many A/B slots the device has.
    pub slot_count: u8,
    /// The slot the device can currently boot, or `OB_SLOT_ID_NONE`.
    pub active_slot: u8,
    /// The slot this session may mutate — always the one that is NOT
    /// active, so an interrupted update leaves the previous image intact.
    pub write_slot: u8,
    /// Where an image for `write_slot` must start. Authoritative: never
    /// derive it from `app_start` and the slot index.
    pub write_base: u32,
    /// Largest image `write_slot` accepts, or 0 when this silicon cannot
    /// hold that slot at all (a wrong-variant build; every mutation is
    /// then refused, so the flows must say so rather than let the device
    /// answer with a bare range error).
    pub write_capacity: u32,
}

fn le16(b: &[u8]) -> u16 {
    u16::from_le_bytes([b[0], b[1]])
}

fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

fn le64(b: &[u8]) -> u64 {
    u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
}

impl DeviceInfo {
    /// Parse a HELLO response payload (status byte included at offset 0).
    /// Rejects short payloads and device info the flows cannot safely use
    /// (zero erase block, unusable write geometry, inverted app region).
    pub fn parse(payload: &[u8]) -> Result<DeviceInfo> {
        if payload.len() < OB_HELLO_RESP_LEN {
            bail!(
                "HELLO response payload is {} bytes; the device must send at least {}",
                payload.len(),
                OB_HELLO_RESP_LEN
            );
        }
        let info = DeviceInfo {
            proto_major: payload[1],
            proto_minor: payload[2],
            chip_rev: payload[3],
            bl_version: le16(&payload[4..6]),
            chip_family: payload[6],
            transport: payload[7],
            app_start: le32(&payload[8..12]),
            app_end: le32(&payload[12..16]),
            erase_block: le32(&payload[16..20]),
            write_page: le16(&payload[20..22]),
            write_align: payload[22],
            max_write_data: payload[23],
            features: le32(&payload[24..28]),
            uid: le64(&payload[28..36]),
            slot_count: payload[36],
            active_slot: payload[37],
            write_slot: payload[38],
            write_base: le32(&payload[40..44]),
            write_capacity: le32(&payload[44..48]),
        };
        if info.app_start >= info.app_end {
            bail!(
                "device reported an inverted app region 0x{:08X}..0x{:08X}",
                info.app_start,
                info.app_end
            );
        }
        if info.erase_block == 0 {
            bail!("device reported a zero erase block size");
        }
        if info.write_align == 0 {
            bail!("device reported a zero write alignment");
        }
        if info.max_write_data == 0 || !usize::from(info.max_write_data).is_multiple_of(4) {
            bail!(
                "device reported an unusable max_write_data of {} bytes",
                info.max_write_data
            );
        }
        if usize::from(info.max_write_data) > OB_MAX_WRITE_DATA {
            bail!(
                "device reported max_write_data of {} bytes, above the protocol maximum of {}",
                info.max_write_data,
                OB_MAX_WRITE_DATA
            );
        }
        // Slot coherence. A zero capacity is NOT rejected here: it is a
        // truthful report from a device whose silicon cannot hold the slot,
        // and `probe` has to be able to show it. The mutating flows refuse
        // it by name instead.
        if info.slot_count == 0 {
            bail!("device reported zero slots");
        }
        if info.write_slot >= info.slot_count {
            bail!(
                "device reported write slot {} with only {} slot(s)",
                info.write_slot,
                info.slot_count
            );
        }
        if info.active_slot != OB_SLOT_ID_NONE && info.active_slot >= info.slot_count {
            bail!(
                "device reported active slot {} with only {} slot(s)",
                info.active_slot,
                info.slot_count
            );
        }
        if info.write_base < info.app_start
            || u64::from(info.write_base) + u64::from(info.write_capacity) > u64::from(info.app_end)
        {
            bail!(
                "device reported a write window 0x{:08X}..0x{:08X} outside its app region \
                 0x{:08X}..0x{:08X}",
                info.write_base,
                u64::from(info.write_base) + u64::from(info.write_capacity),
                info.app_start,
                info.app_end
            );
        }
        Ok(info)
    }

    /// Human name for a slot id: "A", "B", or "none" for OB_SLOT_ID_NONE.
    pub fn slot_name(slot: u8) -> String {
        match slot {
            OB_SLOT_ID_NONE => "none".to_string(),
            0..=25 => ((b'A' + slot) as char).to_string(),
            other => other.to_string(),
        }
    }

    pub fn family_name(&self) -> String {
        match self.chip_family {
            OB_FAMILY_CH570 => "CH570".to_string(),
            OB_FAMILY_CH572 => "CH572".to_string(),
            OB_FAMILY_CH591 => "CH591".to_string(),
            OB_FAMILY_CH592 => "CH592".to_string(),
            other => format!("unknown family 0x{other:02X}"),
        }
    }

    /// The chip-id byte this family should report, if we know it.
    ///
    /// `chip_family` is a BUILD-time constant baked into the image, while
    /// `chip_rev` is read from silicon at run time. If they disagree, the
    /// image was built for a different variant than the part it is running
    /// on — worth saying out loud before anyone erases anything, because the
    /// two parts can have different flash sizes.
    fn expected_chip_rev(&self) -> Option<u8> {
        match self.chip_family {
            OB_FAMILY_CH570 => Some(0x70),
            OB_FAMILY_CH572 => Some(0x72),
            OB_FAMILY_CH591 => Some(0x91),
            OB_FAMILY_CH592 => Some(0x92),
            _ => None,
        }
    }

    /// `Some(warning)` when the image and the silicon disagree.
    ///
    /// Not an error: the firmware clamps its own app region to the silicon,
    /// so the device is safe either way, and an unrecognised pairing may
    /// simply be a variant newer than this tool. But it is always worth
    /// reporting — it means someone is about to flash the wrong artifact.
    pub fn variant_mismatch(&self) -> Option<String> {
        let want = self.expected_chip_rev()?;
        if self.chip_rev == want {
            return None;
        }
        Some(format!(
            "image is built for {} (expects chip id 0x{:02X}) but the silicon \
reports chip id 0x{:02X} — you are probably flashing the wrong variant; the \
device has clamped its app region to what the part actually has",
            self.family_name(),
            want,
            self.chip_rev
        ))
    }

    pub fn transport_name(&self) -> String {
        match self.transport {
            OB_TRANSPORT_ID_USB => "usb".to_string(),
            OB_TRANSPORT_ID_UART => "uart".to_string(),
            other => format!("unknown transport 0x{other:02X}"),
        }
    }

    /// 64-bit ROM UID as 16 upper-case hex digits (matches the USB iSerial).
    pub fn uid_hex(&self) -> String {
        format!("{:016X}", self.uid)
    }

    pub fn has_feature(&self, bit: u32) -> bool {
        self.features & bit != 0
    }

    /// CRC command is authoritative for flash written this power cycle.
    /// Clear on CH57x, where XIP may serve stale data (errata F26).
    pub fn crc_live(&self) -> bool {
        self.has_feature(OB_FEAT_CRC_LIVE)
    }

    pub fn feature_names(&self) -> String {
        let mut names = Vec::new();
        if self.has_feature(OB_FEAT_READ) {
            names.push("READ".to_string());
        }
        if self.has_feature(OB_FEAT_CRC_LIVE) {
            names.push("CRC_LIVE".to_string());
        }
        let unknown = self.features & !(OB_FEAT_READ | OB_FEAT_CRC_LIVE);
        if unknown != 0 {
            names.push(format!("unknown:0x{unknown:X}"));
        }
        if names.is_empty() {
            "none".to_string()
        } else {
            names.join(" ")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::consts::{OB_PROTO_MAJOR, OB_PROTO_MINOR};

    /// A CH592 image on a CH591 is the dangerous pairing: same port, same
    /// family code path, but 448 KiB advertised on a 192 KiB die.
    #[test]
    fn variant_mismatch_flags_a_wrong_artifact() {
        let mut info = DeviceInfo::parse(&valid_payload()).unwrap();
        info.chip_rev = 0x91; // silicon says CH591, image says CH592
        let w = info.variant_mismatch().expect("mismatch must be reported");
        assert!(w.contains("CH592"), "{w}");
        assert!(w.contains("0x91"), "{w}");
    }

    #[test]
    fn variant_mismatch_is_quiet_when_they_agree() {
        let mut info = DeviceInfo::parse(&valid_payload()).unwrap();
        for (fam, rev) in [
            (OB_FAMILY_CH570, 0x70u8),
            (OB_FAMILY_CH572, 0x72),
            (OB_FAMILY_CH591, 0x91),
            (OB_FAMILY_CH592, 0x92),
        ] {
            info.chip_family = fam;
            info.chip_rev = rev;
            assert!(info.variant_mismatch().is_none(), "0x{fam:02X}/0x{rev:02X}");
        }
    }

    /// A variant newer than this tool must not produce a scary warning.
    #[test]
    fn variant_mismatch_is_quiet_on_an_unknown_family() {
        let mut info = DeviceInfo::parse(&valid_payload()).unwrap();
        info.chip_family = 0x7F;
        info.chip_rev = 0x7F;
        assert!(info.variant_mismatch().is_none());
    }

    fn valid_payload() -> Vec<u8> {
        let mut p = vec![0x00, OB_PROTO_MAJOR, OB_PROTO_MINOR, 9];
        p.extend_from_slice(&0x000Au16.to_le_bytes());
        p.push(OB_FAMILY_CH592);
        p.push(OB_TRANSPORT_ID_USB);
        p.extend_from_slice(&0x2000u32.to_le_bytes());
        p.extend_from_slice(&0x70000u32.to_le_bytes());
        p.extend_from_slice(&4096u32.to_le_bytes());
        p.extend_from_slice(&256u16.to_le_bytes());
        p.push(4);
        p.push(48);
        p.extend_from_slice(&OB_FEAT_CRC_LIVE.to_le_bytes());
        p.extend_from_slice(&0x0123_4567_89AB_CDEFu64.to_le_bytes());
        p.extend_from_slice(&[2, OB_SLOT_ID_NONE, 0, 0]);
        p.extend_from_slice(&0x2000u32.to_le_bytes());
        p.extend_from_slice(&0x0003_6000u32.to_le_bytes());
        assert_eq!(p.len(), OB_HELLO_RESP_LEN);
        p
    }

    #[test]
    fn short_payload_rejected() {
        let err = DeviceInfo::parse(&valid_payload()[..OB_HELLO_RESP_LEN - 1]).unwrap_err();
        let want = format!("{} bytes", OB_HELLO_RESP_LEN - 1);
        assert!(err.to_string().contains(&want), "got: {err}");
    }

    #[test]
    fn longer_payload_tolerated() {
        let mut p = valid_payload();
        p.extend_from_slice(&[0xEE; 8]); // future extension bytes
        let info = DeviceInfo::parse(&p).unwrap();
        assert_eq!(info.uid, 0x0123_4567_89AB_CDEF);
    }

    #[test]
    fn nonsense_geometry_rejected() {
        let mut p = valid_payload();
        p[16..20].copy_from_slice(&0u32.to_le_bytes()); // erase_block = 0
        assert!(DeviceInfo::parse(&p).is_err());

        let mut p = valid_payload();
        p[12..16].copy_from_slice(&0x2000u32.to_le_bytes()); // app_end == app_start
        assert!(DeviceInfo::parse(&p).is_err());
    }

    #[test]
    fn max_write_data_geometry_is_enforced() {
        for value in [0, 7] {
            let mut p = valid_payload();
            p[23] = value;
            let err = DeviceInfo::parse(&p).unwrap_err();
            assert!(err.to_string().contains("unusable max_write_data"), "{err}");
        }

        for value in [52, 56] {
            let mut p = valid_payload();
            p[23] = value;
            let err = DeviceInfo::parse(&p).unwrap_err();
            assert!(err.to_string().contains("protocol maximum"), "{err}");
        }

        let info = DeviceInfo::parse(&valid_payload()).unwrap();
        assert_eq!(usize::from(info.max_write_data), OB_MAX_WRITE_DATA);
    }

    #[test]
    fn names_and_features() {
        let info = DeviceInfo::parse(&valid_payload()).unwrap();
        assert_eq!(info.family_name(), "CH592");
        assert_eq!(info.transport_name(), "usb");
        assert_eq!(info.uid_hex(), "0123456789ABCDEF");
        assert!(info.crc_live());
        assert!(!info.has_feature(OB_FEAT_READ));
        assert_eq!(info.feature_names(), "CRC_LIVE");

        let mut p = valid_payload();
        p[24..28].copy_from_slice(&0u32.to_le_bytes());
        let bare = DeviceInfo::parse(&p).unwrap();
        assert_eq!(bare.feature_names(), "none");

        let mut p = valid_payload();
        p[24..28].copy_from_slice(&0x13u32.to_le_bytes());
        let exotic = DeviceInfo::parse(&p).unwrap();
        assert_eq!(exotic.feature_names(), "READ CRC_LIVE unknown:0x10");
    }
}
