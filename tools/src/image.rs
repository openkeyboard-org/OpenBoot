//! Firmware image loading: flat `.bin` (default base = app base 0x2000,
//! `--base` override) and Intel HEX (base comes from the records; decoding
//! lives in [`crate::hex`]). Loaded images are padded to a 4-byte multiple
//! with 0xFF (flash writes are 4-byte units); region bounds are checked in
//! `flows` against the device-reported app region after HELLO.

use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::hex::parse_intel_hex;
use crate::proto::consts::OB_APP_BASE;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub base: u32,
    pub bytes: Vec<u8>,
}

impl Image {
    /// CRC-32/ISO-HDLC over the (padded) image bytes — the value COMMIT
    /// attests and the CRC command is compared against.
    pub fn crc32(&self) -> u32 {
        crc32fast::hash(&self.bytes)
    }
}

fn pad4(mut bytes: Vec<u8>) -> Vec<u8> {
    let rem = bytes.len() % 4;
    if rem != 0 {
        bytes.resize(bytes.len() + (4 - rem), 0xFF);
    }
    bytes
}

/// Load a firmware file. `.hex` is parsed as Intel HEX (its records set the
/// base, so `--base` is rejected); anything else is a flat binary based at
/// `base_override` or the shared app base 0x2000. The result is padded to a
/// 4-byte multiple with 0xFF.
pub fn load_image(path: &Path, base_override: Option<u32>) -> Result<Image> {
    let raw = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let is_hex = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("hex"));
    if is_hex {
        if base_override.is_some() {
            bail!(
                "--base applies to raw .bin images only; {} is Intel HEX and \
                 its records set the load address",
                path.display()
            );
        }
        let text = String::from_utf8_lossy(&raw);
        let (base, img) =
            parse_intel_hex(&text).with_context(|| format!("parse {}", path.display()))?;
        if img.is_empty() {
            bail!("{}: no data records", path.display());
        }
        Ok(Image {
            base,
            bytes: pad4(img),
        })
    } else {
        if raw.is_empty() {
            bail!("{} is empty", path.display());
        }
        Ok(Image {
            base: base_override.unwrap_or(OB_APP_BASE),
            bytes: pad4(raw),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(name: &str, content: &[u8]) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(name);
        fs::write(&p, content).unwrap();
        p
    }

    #[test]
    fn bin_pads_to_four_bytes_at_default_base() {
        let p = temp_file("openboot_pad_test.bin", &[0xAA, 0xBB, 0xCC, 0xDD, 0xEE]);
        let img = load_image(&p, None).unwrap();
        assert_eq!(img.base, 0x2000);
        assert_eq!(
            img.bytes,
            vec![0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0xFF, 0xFF]
        );
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn bin_base_override() {
        let p = temp_file("openboot_base_test.bin", &[1, 2, 3, 4]);
        let img = load_image(&p, Some(0x2000)).unwrap();
        assert_eq!(img.base, 0x2000);
        assert_eq!(img.bytes, vec![1, 2, 3, 4]); // already aligned: no padding
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn empty_bin_rejected() {
        let p = temp_file("openboot_empty_test.bin", &[]);
        assert!(load_image(&p, None).is_err());
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn hex_file_loads_and_pads() {
        // 6 data bytes at 0x2000 -> padded to 8. Build the record with a
        // computed checksum rather than hand-writing it.
        let mut rec = vec![0x06u8, 0x20, 0x00, 0x00, 1, 2, 3, 4, 5, 6];
        let sum: u8 = rec.iter().fold(0u8, |a, b| a.wrapping_add(*b));
        rec.push(sum.wrapping_neg());
        let line: String = rec.iter().map(|b| format!("{b:02X}")).collect();
        let full = format!(":{line}\n:00000001FF\n");
        let p = temp_file("openboot_hex_test.hex", full.as_bytes());
        let img = load_image(&p, None).unwrap();
        assert_eq!(img.base, 0x2000);
        assert_eq!(img.bytes, vec![1, 2, 3, 4, 5, 6, 0xFF, 0xFF]);
        assert_eq!(
            img.crc32(),
            crc32fast::hash(&[1, 2, 3, 4, 5, 6, 0xFF, 0xFF])
        );
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn hex_with_base_override_rejected() {
        let p = temp_file("openboot_hexbase_test.hex", b":00000001FF\n");
        let err = load_image(&p, Some(0x1000)).unwrap_err();
        assert!(err.to_string().contains("--base"), "got: {err}");
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn hex_with_no_data_records_rejected() {
        let p = temp_file("openboot_nodata_test.hex", b":00000001FF\n");
        let err = load_image(&p, None).unwrap_err();
        assert!(err.to_string().contains("no data records"), "got: {err}");
        let _ = fs::remove_file(&p);
    }
}
