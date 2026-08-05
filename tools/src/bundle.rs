//! Slot bundles: one release file carrying every per-slot build of an app.
//!
//! A/B slots are not copies of one image. These parts have no flash remap and
//! the vendor blobs cannot be relocated, so an application is LINKED for a
//! slot base and only runs there (see `docs/AB-UPDATE.md`). The base the
//! device will accept alternates with every update, so "the artifact to
//! flash" is not a fixed file — which a flat `.bin` cannot express at all,
//! since it carries no base and the tool has to be told one by hand.
//!
//! A bundle carries the variants together with the base each was linked for.
//! The tool then asks the device which slot it is writing and picks the
//! matching variant, so a release is one file with one digest and no operator
//! decision at flash time.
//!
//! Layout, all little-endian, 4-byte aligned throughout:
//!
//! ```text
//!   offset  size      field
//!   0       4         magic "OBB1"
//!   4       1         format_major (1) — a bump means incompatible
//!   5       1         format_minor (0) — additive only
//!   6       1         chip_family (OB_FAMILY_*, or 0 = unspecified)
//!   7       1         variant_count (1..=MAX_VARIANTS)
//!   8       4         crc32 of the WHOLE file with these four bytes zeroed
//!   12      4         reserved, MUST be zero
//!   16      n*16      variant table, ascending by base:
//!                       0  4  base    link address
//!                       4  4  len     payload bytes
//!                       8  4  crc32   over the payload
//!                       12 4  offset  from file start
//!   ...               payloads
//! ```
//!
//! The whole-file CRC is deliberately coarse: it covers the header, the
//! table and every payload in one value, so a truncated or edited bundle
//! fails before any of it is believed. Per-variant CRCs are still stored
//! because they are what COMMIT attests, so the tool can hand the device a
//! value it has independently checked.

use anyhow::{bail, Context, Result};

use crate::image::Image;
use crate::proto::consts::{OB_FAMILY_CH570, OB_FAMILY_CH572, OB_FAMILY_CH591, OB_FAMILY_CH592};

pub const MAGIC: &[u8; 4] = b"OBB1";
pub const FORMAT_MAJOR: u8 = 1;
pub const FORMAT_MINOR: u8 = 0;
pub const HEADER_LEN: usize = 16;
pub const ENTRY_LEN: usize = 16;
/// Two slots today. The cap exists so a corrupt count cannot make the parser
/// allocate wildly before the CRC has been checked.
pub const MAX_VARIANTS: usize = 8;
/// `chip_family` when the producer did not say.
pub const FAMILY_UNSPECIFIED: u8 = 0;

pub const FAMILY_NAMES: &[(&str, u8)] = &[
    ("ch570", OB_FAMILY_CH570),
    ("ch572", OB_FAMILY_CH572),
    ("ch591", OB_FAMILY_CH591),
    ("ch592", OB_FAMILY_CH592),
];

pub fn family_from_name(name: &str) -> Option<u8> {
    FAMILY_NAMES
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| *v)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bundle {
    /// `OB_FAMILY_*`, or `FAMILY_UNSPECIFIED`. Cross-checked against HELLO so
    /// a CH592 release cannot be flashed onto a CH570 that would accept the
    /// addresses but not run the code.
    pub chip_family: u8,
    /// One per slot, ascending by base and non-overlapping.
    pub images: Vec<Image>,
}

fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

impl Bundle {
    pub fn new(chip_family: u8, mut images: Vec<Image>) -> Result<Bundle> {
        if images.is_empty() {
            bail!("a bundle needs at least one image");
        }
        if images.len() > MAX_VARIANTS {
            bail!(
                "{} images exceeds the bundle maximum of {MAX_VARIANTS}",
                images.len()
            );
        }
        // The same rules parse() enforces, applied here too. Without them
        // `bundle create` happily writes a file it will refuse to read back:
        // a misaligned base survives encode() and fails on the next parse.
        for (i, img) in images.iter().enumerate() {
            if img.base % 4 != 0 {
                bail!("image {i} base 0x{:08X} is not 4-byte aligned", img.base);
            }
            if img.bytes.is_empty() || img.bytes.len() % 4 != 0 {
                bail!(
                    "image {i} is {} bytes, not a nonzero multiple of 4 \
                     (flash writes are 4-byte units)",
                    img.bytes.len()
                );
            }
        }
        images.sort_by_key(|i| i.base);
        for pair in images.windows(2) {
            let (a, b) = (&pair[0], &pair[1]);
            if a.base == b.base {
                bail!("two images share base 0x{:08X}", a.base);
            }
            // Overlap means one slot's image would run into the next; the
            // device would refuse it, but saying so here names both files.
            let a_end = u64::from(a.base) + a.bytes.len() as u64;
            if a_end > u64::from(b.base) {
                bail!(
                    "image at 0x{:08X} is {} bytes and runs into the image at 0x{:08X}",
                    a.base,
                    a.bytes.len(),
                    b.base
                );
            }
        }
        Ok(Bundle {
            chip_family,
            images,
        })
    }

    pub fn encode(&self) -> Vec<u8> {
        let table_len = self.images.len() * ENTRY_LEN;
        let mut out = Vec::with_capacity(HEADER_LEN + table_len);
        out.extend_from_slice(MAGIC);
        out.push(FORMAT_MAJOR);
        out.push(FORMAT_MINOR);
        out.push(self.chip_family);
        out.push(self.images.len() as u8);
        out.extend_from_slice(&0u32.to_le_bytes()); // crc32, filled below
        out.extend_from_slice(&0u32.to_le_bytes()); // reserved

        let mut offset = (HEADER_LEN + table_len) as u32;
        for img in &self.images {
            out.extend_from_slice(&img.base.to_le_bytes());
            out.extend_from_slice(&(img.bytes.len() as u32).to_le_bytes());
            out.extend_from_slice(&img.crc32().to_le_bytes());
            out.extend_from_slice(&offset.to_le_bytes());
            offset += img.bytes.len() as u32;
        }
        for img in &self.images {
            out.extend_from_slice(&img.bytes);
        }

        let crc = crc32fast::hash(&out);
        out[8..12].copy_from_slice(&crc.to_le_bytes());
        out
    }

    /// Is this file a bundle? Checked by magic alone, so an ordinary `.bin`
    /// is never mistaken for one and a corrupt bundle still reports as a
    /// bundle (and then fails parsing with a reason).
    pub fn looks_like_bundle(raw: &[u8]) -> bool {
        raw.len() >= 4 && &raw[..4] == MAGIC
    }

    pub fn parse(raw: &[u8]) -> Result<Bundle> {
        if raw.len() < HEADER_LEN {
            bail!("too short to be a bundle ({} bytes)", raw.len());
        }
        if &raw[..4] != MAGIC {
            bail!("not a bundle: bad magic");
        }
        let (major, minor) = (raw[4], raw[5]);
        if major != FORMAT_MAJOR {
            bail!(
                "bundle format {major}.{minor} is not supported by this tool \
                 (expected major {FORMAT_MAJOR}); a major bump is incompatible"
            );
        }
        let chip_family = raw[6];
        let count = raw[7] as usize;
        if count == 0 || count > MAX_VARIANTS {
            bail!("bundle declares {count} variants (allowed 1..={MAX_VARIANTS})");
        }
        if le32(&raw[12..16]) != 0 {
            bail!("bundle reserved field is not zero");
        }

        // Verify the whole file before believing any of the table: everything
        // below indexes with values that come out of it.
        let stated = le32(&raw[8..12]);
        let mut zeroed = raw.to_vec();
        zeroed[8..12].fill(0);
        let actual = crc32fast::hash(&zeroed);
        if stated != actual {
            bail!("bundle CRC mismatch: stored 0x{stated:08X}, computed 0x{actual:08X}");
        }

        let table_end = HEADER_LEN + count * ENTRY_LEN;
        if raw.len() < table_end {
            bail!("bundle truncated: {count} variants need {table_end} bytes of header");
        }

        let mut images = Vec::with_capacity(count);
        for i in 0..count {
            let e = &raw[HEADER_LEN + i * ENTRY_LEN..HEADER_LEN + (i + 1) * ENTRY_LEN];
            let (base, len, crc, off) = (
                le32(&e[0..4]),
                le32(&e[4..8]),
                le32(&e[8..12]),
                le32(&e[12..16]),
            );
            if len == 0 || len % 4 != 0 {
                bail!("variant {i} length {len} is not a nonzero multiple of 4");
            }
            if base % 4 != 0 {
                bail!("variant {i} base 0x{base:08X} is not 4-byte aligned");
            }
            let end = off as u64 + len as u64;
            if (off as usize) < table_end || end > raw.len() as u64 {
                bail!("variant {i} payload [{off}, {end}) lies outside the bundle");
            }
            let bytes = raw[off as usize..end as usize].to_vec();
            let got = crc32fast::hash(&bytes);
            if got != crc {
                bail!("variant {i} (base 0x{base:08X}) CRC mismatch: stored 0x{crc:08X}, computed 0x{got:08X}");
            }
            images.push(Image { base, bytes });
        }
        // Re-runs the ordering and overlap rules on parse, so a hand-built
        // bundle gets the same treatment as one this tool wrote.
        Bundle::new(chip_family, images).context("bundle contents")
    }

    pub fn for_base(&self, base: u32) -> Option<&Image> {
        self.images.iter().find(|i| i.base == base)
    }

    pub fn bases(&self) -> Vec<u32> {
        self.images.iter().map(|i| i.base).collect()
    }

    pub fn family_name(&self) -> String {
        match self.chip_family {
            FAMILY_UNSPECIFIED => "unspecified".to_string(),
            f => FAMILY_NAMES
                .iter()
                .find(|(_, v)| *v == f)
                .map(|(n, _)| n.to_uppercase())
                .unwrap_or_else(|| format!("unknown 0x{f:02X}")),
        }
    }
}

/// What the user handed us to flash: one image, or a bundle from which the
/// device's answer picks one.
///
/// The choice cannot be made at load time — it depends on which slot the
/// device is willing to write, which is only known after HELLO — so the flows
/// carry this and resolve it once they have `DeviceInfo`.
#[derive(Debug, Clone)]
pub enum Source {
    Single(Image),
    Bundle(Bundle),
}

impl Source {
    pub fn describe(&self) -> String {
        match self {
            Source::Single(i) => format!(
                "base 0x{:08X}, {} bytes, crc32 0x{:08X}",
                i.base,
                i.bytes.len(),
                i.crc32()
            ),
            Source::Bundle(b) => format!(
                "bundle for {}, {} variant(s) at {}",
                b.family_name(),
                b.images.len(),
                b.bases()
                    .iter()
                    .map(|b| format!("0x{b:08X}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }

    /// Refuse a bundle built for a different part before anything is erased.
    /// A wrong-family image can share the same addresses and still be
    /// unrunnable, and the device cannot tell — it validates addresses, a
    /// length and a CRC, never what the code is.
    pub fn check_family(&self, chip_family: u8, device_name: &str) -> Result<()> {
        if let Source::Bundle(b) = self {
            if b.chip_family != FAMILY_UNSPECIFIED && b.chip_family != chip_family {
                bail!(
                    "bundle is built for {} but the device reports {device_name}",
                    b.family_name()
                );
            }
        }
        Ok(())
    }

    /// The image for a given slot base, or an error naming what is on offer.
    pub fn for_base(&self, base: u32, whose: &str) -> Result<&Image> {
        match self {
            Source::Single(i) => Ok(i),
            Source::Bundle(b) => b.for_base(base).ok_or_else(|| {
                anyhow::anyhow!(
                    "bundle has no variant linked for the {whose} base 0x{base:08X}; \
                     it carries {}. An application cannot be relocated between \
                     slots, so the build for that base has to be in the bundle.",
                    b.bases()
                        .iter()
                        .map(|b| format!("0x{b:08X}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }),
        }
    }

    /// The image to compare against what the device is currently running.
    ///
    /// Chosen WITHOUT computing the active slot's address. The device reports
    /// which base it would write, never where the active image sits, and the
    /// protocol tells hosts not to derive slot addresses themselves. With two
    /// slots and two variants the running one is simply the variant that is
    /// not the write target, which needs no arithmetic and cannot be off by a
    /// slot.
    pub fn for_running_image(&self, write_base: u32) -> Result<&Image> {
        match self {
            Source::Single(i) => Ok(i),
            Source::Bundle(b) => {
                let others: Vec<&Image> =
                    b.images.iter().filter(|i| i.base != write_base).collect();
                match others.as_slice() {
                    [only] => Ok(only),
                    // Deliberately no shortcut for a one-variant bundle. If
                    // its build is the write target, handing it back would
                    // make verify CRC the slot about to be OVERWRITTEN and
                    // report OK about an image nothing is running.
                    [] => bail!(
                        "the bundle only carries the build for 0x{write_base:08X}, \
                         which is the slot the device is about to WRITE, not the \
                         one it is running — verifying it would report on the \
                         wrong slot"
                    ),
                    _ => bail!(
                        "cannot tell which variant is running: the bundle carries \
                         {} builds besides the write target 0x{write_base:08X}. \
                         Pass the single image for the slot you want to check.",
                        others.len()
                    ),
                }
            }
        }
    }
}

/// Load a flashable: a bundle if the file carries the bundle magic, otherwise
/// a single image as before.
pub fn load_source(path: &std::path::Path, base_override: Option<u32>) -> Result<Source> {
    let raw = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    if Bundle::looks_like_bundle(&raw) {
        if base_override.is_some() {
            bail!(
                "--base does not apply to {}: a bundle carries the base each \
                 variant was linked for",
                path.display()
            );
        }
        let b = Bundle::parse(&raw).with_context(|| format!("parse {}", path.display()))?;
        return Ok(Source::Bundle(b));
    }
    Ok(Source::Single(crate::image::load_image(
        path,
        base_override,
    )?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(base: u32, len: usize, fill: u8) -> Image {
        Image {
            base,
            bytes: vec![fill; len],
        }
    }

    fn two() -> Bundle {
        Bundle::new(
            OB_FAMILY_CH592,
            vec![img(0x39000, 64, 0xBB), img(0x2000, 48, 0xAA)],
        )
        .unwrap()
    }

    #[test]
    fn round_trips_and_sorts_by_base() {
        let b = two();
        assert_eq!(
            b.bases(),
            vec![0x2000, 0x39000],
            "input order must not matter"
        );
        let back = Bundle::parse(&b.encode()).unwrap();
        assert_eq!(back, b);
        assert_eq!(back.chip_family, OB_FAMILY_CH592);
    }

    #[test]
    fn a_single_flipped_bit_anywhere_is_caught() {
        let good = two().encode();
        // Header, table and payload each in turn — the whole-file CRC exists
        // so that no region of the file is believed without being checked.
        for at in [6, HEADER_LEN + 2, good.len() - 1] {
            let mut bad = good.clone();
            bad[at] ^= 0x01;
            let err = Bundle::parse(&bad).unwrap_err().to_string();
            assert!(
                err.contains("CRC mismatch") || err.contains("variants"),
                "byte {at} not caught: {err}"
            );
        }
    }

    #[test]
    fn truncation_is_caught() {
        let good = two().encode();
        let err = Bundle::parse(&good[..good.len() - 8])
            .unwrap_err()
            .to_string();
        assert!(err.contains("CRC mismatch"), "got: {err}");
    }

    #[test]
    fn a_plain_binary_is_not_mistaken_for_a_bundle() {
        assert!(!Bundle::looks_like_bundle(&[0xFF; 64]));
        assert!(!Bundle::looks_like_bundle(b"OB"));
        assert!(Bundle::looks_like_bundle(&two().encode()));
    }

    #[test]
    fn an_incompatible_major_is_refused() {
        let mut raw = two().encode();
        raw[4] = FORMAT_MAJOR + 1;
        let crc = crc32fast::hash(&{
            let mut z = raw.clone();
            z[8..12].fill(0);
            z
        });
        raw[8..12].copy_from_slice(&crc.to_le_bytes());
        let err = Bundle::parse(&raw).unwrap_err().to_string();
        assert!(err.contains("not supported"), "got: {err}");
    }

    #[test]
    fn overlapping_and_duplicate_slots_are_refused() {
        let err = Bundle::new(0, vec![img(0x2000, 0x1000, 1), img(0x2800, 16, 2)])
            .unwrap_err()
            .to_string();
        assert!(err.contains("runs into"), "got: {err}");

        let err = Bundle::new(0, vec![img(0x2000, 16, 1), img(0x2000, 16, 2)])
            .unwrap_err()
            .to_string();
        assert!(err.contains("share base"), "got: {err}");
    }

    #[test]
    fn what_it_writes_it_can_read_back() {
        // The round trip is the property: a bundle create emits must parse.
        for bad in [
            Bundle::new(0, vec![img(0x2001, 16, 1)]),
            Bundle::new(0, vec![img(0x2000, 15, 1)]),
            Bundle::new(
                0,
                vec![Image {
                    base: 0x2000,
                    bytes: vec![],
                }],
            ),
        ] {
            assert!(bad.is_err(), "construction must reject what parse would");
        }
        let good = two();
        assert!(Bundle::parse(&good.encode()).is_ok());
    }

    #[test]
    fn selection_is_by_exact_base() {
        let b = two();
        assert_eq!(b.for_base(0x39000).unwrap().bytes.len(), 64);
        assert!(b.for_base(0x2004).is_none(), "a near miss is not a match");
    }

    #[test]
    fn a_lone_variant_for_the_write_slot_is_not_treated_as_running() {
        let only_b = Bundle::new(0, vec![img(0x39000, 16, 0xBB)]).unwrap();
        // Device is writing slot B, and slot B's build is all the bundle has.
        // Returning it means verify CRCs the slot being WRITTEN.
        let err = Source::Bundle(only_b)
            .for_running_image(0x39000)
            .unwrap_err()
            .to_string();
        assert!(err.contains("about to WRITE"), "got: {err}");

        // The same lone variant IS the running one when the device is
        // writing the other slot.
        let only_b = Bundle::new(0, vec![img(0x39000, 16, 0xBB)]).unwrap();
        assert_eq!(
            Source::Bundle(only_b)
                .for_running_image(0x2000)
                .unwrap()
                .base,
            0x39000
        );
    }

    #[test]
    fn the_running_image_is_the_one_not_being_written() {
        let src = Source::Bundle(two());
        // Device is writing slot B, so slot A is what is running.
        assert_eq!(src.for_running_image(0x39000).unwrap().base, 0x2000);
        assert_eq!(src.for_running_image(0x2000).unwrap().base, 0x39000);
    }

    #[test]
    fn a_missing_variant_names_what_is_on_offer() {
        let src = Source::Bundle(two());
        let err = src
            .for_base(0x1F000, "device write")
            .unwrap_err()
            .to_string();
        assert!(err.contains("0x0001F000"), "got: {err}");
        assert!(
            err.contains("0x00002000") && err.contains("0x00039000"),
            "got: {err}"
        );
    }

    #[test]
    fn a_bundle_for_another_part_is_refused() {
        let src = Source::Bundle(two());
        assert!(src.check_family(OB_FAMILY_CH592, "CH592").is_ok());
        let err = src
            .check_family(OB_FAMILY_CH570, "CH570")
            .unwrap_err()
            .to_string();
        assert!(err.contains("CH592") && err.contains("CH570"), "got: {err}");
    }

    #[test]
    fn an_unspecified_family_matches_anything() {
        let b = Bundle::new(FAMILY_UNSPECIFIED, vec![img(0x2000, 16, 1)]).unwrap();
        assert!(Source::Bundle(b)
            .check_family(OB_FAMILY_CH570, "CH570")
            .is_ok());
    }

    #[test]
    fn a_single_image_is_used_whatever_the_device_says() {
        let src = Source::Single(img(0x2000, 16, 1));
        // flows still enforce base == write_base for a single image; Source
        // itself must not second-guess that, or the error would move and lose
        // its explanation.
        assert_eq!(src.for_base(0x39000, "device write").unwrap().base, 0x2000);
        assert_eq!(src.for_running_image(0x39000).unwrap().base, 0x2000);
    }
}
