//! Intel HEX decoding, split into two concerns:
//!
//! * [`HexRecord::parse_line`] — one line of text to one typed record. It
//!   owns syntax, exact length, and the record checksum, and NOTHING else.
//! * [`Accumulator`] — addressing (segment/linear bases), the EOF policy,
//!   overlap detection, the span guard, and the final gap-filled image.
//!
//! Ported from OpenDongle `tools/src/hex.rs`, minus that project's
//! ODG2/factory-image validation, plus overlapping-record rejection.
//!
//! The strictness is deliberate and safety-relevant: a silently skipped or
//! truncated record becomes a 0xFF gap that gets flashed as if it were the
//! firmware. Every malformed input is an error, never a warning. The check
//! ORDER inside `parse_line` is also load-bearing — for a line that is bad
//! in more than one way it decides which message the user sees, and the
//! tests assert on those messages.

use anyhow::{anyhow, bail, Result};

/// Refuse absurd hex spans before allocating the gap-filled image.
const MAX_IMAGE_SPAN: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HexRecord {
    /// Type 0: data at `offset` from the current base.
    Data { offset: u16, bytes: Vec<u8> },
    /// Type 1: end of file.
    Eof,
    /// Type 2: extended segment address; base = value << 4.
    ExtendedSegment(u16),
    /// Type 4: extended linear address; base = value << 16.
    ExtendedLinear(u16),
    /// A well-formed record of a type we do not act on (3, 5, ...). The
    /// checksum has still been verified.
    Ignored,
}

fn u32_from_hex(s: &str) -> Result<u32> {
    u32::from_str_radix(s, 16).map_err(|e| anyhow!("invalid hex '{s}': {e}"))
}

fn hex_to_bytes(s: &str) -> Result<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        bail!("odd-length hex field: '{s}'");
    }
    (0..s.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&s[i..i + 2], 16)
                .map_err(|e| anyhow!("invalid hex byte '{}': {e}", &s[i..i + 2]))
        })
        .collect()
}

impl HexRecord {
    /// Decode one non-blank, trimmed line. Callers handle blank lines and
    /// the content-after-EOF rule (that needs cross-line state).
    pub fn parse_line(line: &str) -> Result<HexRecord> {
        if !line.starts_with(':') {
            // A silently skipped line could be a data record that lost its
            // colon; the gap would be 0xFF-filled and flashed as-is.
            bail!("intel hex: junk line (records start with ':'): {line}");
        }
        if !line.is_ascii() {
            bail!("intel hex: non-ASCII record: {line}");
        }
        if line.len() < 11 {
            bail!("intel hex: record too short: {line}");
        }
        let n = u32_from_hex(&line[1..3])?;
        let addr = u32_from_hex(&line[3..7])?;
        let t = u32_from_hex(&line[7..9])?;
        let data_end = 9 + (n as usize) * 2;
        if line.len() < data_end {
            bail!("intel hex: truncated record: {line}");
        }
        // Verify the record checksum: the trailing byte is the two's
        // complement of everything before it, so the record sums to 0 mod 256.
        if line.len() < data_end + 2 {
            bail!("intel hex: record is missing its checksum byte: {line}");
        }
        if line.len() > data_end + 2 {
            // Trailing bytes are most likely a second record whose newline
            // was lost; ignoring them would silently drop that record and
            // 0xFF-fill its range.
            bail!("intel hex: trailing bytes after the record checksum: {line}");
        }
        let checked = hex_to_bytes(&line[1..data_end + 2])?;
        let sum = checked.iter().fold(0u8, |acc, b| acc.wrapping_add(*b));
        if sum != 0 {
            bail!(
                "intel hex: bad record checksum (record bytes sum to 0x{sum:02X}, \
                 expected 0x00): {line}"
            );
        }
        let data_hex = &line[9..data_end];
        match t {
            0 => Ok(HexRecord::Data {
                offset: addr as u16,
                bytes: hex_to_bytes(data_hex)?,
            }),
            1 => Ok(HexRecord::Eof),
            2 => Ok(HexRecord::ExtendedSegment(extended_value(t, n, data_hex)?)),
            4 => Ok(HexRecord::ExtendedLinear(extended_value(t, n, data_hex)?)),
            _ => Ok(HexRecord::Ignored),
        }
    }
}

/// Types 2 and 4 carry exactly two data bytes per the Intel HEX spec.
/// Enforced rather than truncated: a wrong-length base record would shift
/// every following address.
fn extended_value(t: u32, n: u32, data_hex: &str) -> Result<u16> {
    if n != 2 {
        bail!("intel hex: type {t} record must carry exactly 2 data bytes, got {n}");
    }
    Ok(u32_from_hex(data_hex)? as u16)
}

/// Assembles decoded records into one contiguous image.
#[derive(Default)]
struct Accumulator {
    base: u64,
    records: Vec<(u64, Vec<u8>)>,
    min: Option<u64>,
    max: u64,
    saw_eof: bool,
}

impl Accumulator {
    fn feed(&mut self, rec: HexRecord) -> Result<()> {
        match rec {
            HexRecord::Data { offset, bytes } => {
                let full = self.base + u64::from(offset);
                let end = full + bytes.len() as u64;
                if end > u64::from(u32::MAX) + 1 {
                    bail!("intel hex: record at 0x{full:X} exceeds the 32-bit address space");
                }
                self.records.push((full, bytes));
                if self.min.is_none_or(|m| full < m) {
                    self.min = Some(full);
                }
                if end > self.max {
                    self.max = end;
                }
            }
            HexRecord::Eof => self.saw_eof = true,
            HexRecord::ExtendedSegment(v) => self.base = u64::from(v) << 4,
            HexRecord::ExtendedLinear(v) => self.base = u64::from(v) << 16,
            HexRecord::Ignored => {}
        }
        Ok(())
    }

    fn finish(self) -> Result<(u32, Vec<u8>)> {
        if !self.saw_eof {
            bail!("intel hex: missing EOF record — file truncated?");
        }
        let Some(min_a) = self.min else {
            return Ok((0, Vec::new()));
        };
        let span = self.max - min_a;
        if span > MAX_IMAGE_SPAN {
            bail!(
                "intel hex: image spans {span} bytes (0x{min_a:X}..0x{:X}); refusing",
                self.max
            );
        }
        let mut img = vec![0xFFu8; span as usize];
        let mut used = vec![false; span as usize];
        for (a, d) in self.records {
            let off = (a - min_a) as usize;
            for (i, byte) in d.iter().enumerate() {
                if used[off + i] {
                    bail!(
                        "intel hex: overlapping data records at 0x{:08X}",
                        a + i as u64
                    );
                }
                used[off + i] = true;
                img[off + i] = *byte;
            }
        }
        Ok((min_a as u32, img))
    }
}

/// Parse Intel HEX text into `(base_addr, image)`. Gaps between records are
/// filled with 0xFF; type 2/4 records set the segment/linear base; record
/// checksums are verified; overlapping data records are rejected. Returns
/// `(0, [])` for input whose only record is EOF.
pub fn parse_intel_hex(text: &str) -> Result<(u32, Vec<u8>)> {
    let mut acc = Accumulator::default();
    let normalized = text.replace("\r\n", "\n");

    for line in normalized.split('\n') {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if acc.saw_eof {
            bail!("intel hex: content after the EOF record: {line}");
        }
        acc.feed(HexRecord::parse_line(line)?)?;
    }
    acc.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn to_hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    /* --- whole-file parsing (moved verbatim from image.rs) ------------- */

    #[test]
    fn parse_with_gap_fill() {
        let text = ":04000000DEADBEEFC4\r\n:020010001122BB\r\n:00000001FF\r\n";
        let (base, img) = parse_intel_hex(text).unwrap();
        assert_eq!(base, 0x0000);
        assert_eq!(to_hex(&img), "deadbeefffffffffffffffffffffffff1122");
        assert_eq!(img.len(), 18);
    }

    #[test]
    fn parse_extended_linear_address() {
        let golden = ":020000040001F9\n:02000000ABCD86\n:00000001FF\n";
        let (base, img) = parse_intel_hex(golden).unwrap();
        assert_eq!(base, 0x00010000);
        assert_eq!(to_hex(&img), "abcd");
    }

    #[test]
    fn empty_returns_zero() {
        let (base, img) = parse_intel_hex(":00000001FF\n").unwrap();
        assert_eq!(base, 0);
        assert!(img.is_empty());
    }

    #[test]
    fn corrupt_record_checksum_is_rejected() {
        let err = parse_intel_hex(":02000000ABCE86\n:00000001FF\n").unwrap_err();
        assert!(
            err.to_string().contains("bad record checksum"),
            "expected a checksum rejection, got: {err}"
        );
    }

    #[test]
    fn record_missing_its_checksum_is_rejected() {
        let err = parse_intel_hex(":02000000ABCD\n").unwrap_err();
        assert!(
            err.to_string().contains("missing its checksum"),
            "got: {err}"
        );
    }

    #[test]
    fn overlapping_records_are_rejected() {
        // Two records both covering address 0x0001.
        let text = ":02000000ABCD86\n:020001001122CA\n:00000001FF\n";
        let err = parse_intel_hex(text).unwrap_err();
        assert!(
            err.to_string()
                .contains("overlapping data records at 0x00000001"),
            "got: {err}"
        );
    }

    #[test]
    fn adjacent_records_are_not_overlapping() {
        let text = ":02000000ABCD86\n:020002001122C9\n:00000001FF\n";
        let (base, img) = parse_intel_hex(text).unwrap();
        assert_eq!(base, 0);
        assert_eq!(to_hex(&img), "abcd1122");
    }

    #[test]
    fn junk_line_rejected() {
        // A data record that lost its colon must not be silently skipped.
        let text = ":02000000ABCD86\n020010001122BB\n:00000001FF\n";
        let err = parse_intel_hex(text).unwrap_err();
        assert!(err.to_string().contains("junk line"), "got: {err}");
    }

    #[test]
    fn missing_eof_rejected() {
        // Checksum-valid data record, then truncation: no EOF record.
        let err = parse_intel_hex(":02000000ABCD86\n").unwrap_err();
        assert!(err.to_string().contains("missing EOF"), "got: {err}");
    }

    #[test]
    fn content_after_eof_rejected() {
        let text = ":02000000ABCD86\n:00000001FF\n:020010001122BB\n";
        let err = parse_intel_hex(text).unwrap_err();
        assert!(err.to_string().contains("after the EOF"), "got: {err}");
    }

    #[test]
    fn blank_lines_are_fine() {
        let text = "\n:02000000ABCD86\n\n:00000001FF\n\n  \n";
        let (base, img) = parse_intel_hex(text).unwrap();
        assert_eq!(base, 0);
        assert_eq!(to_hex(&img), "abcd");
    }

    #[test]
    fn trailing_bytes_after_checksum_rejected() {
        // Two records concatenated by a lost newline: the second must not
        // silently disappear.
        let text = ":02000000ABCD86:020010001122BB\n:00000001FF\n";
        let err = parse_intel_hex(text).unwrap_err();
        assert!(err.to_string().contains("trailing bytes"), "got: {err}");
    }

    #[test]
    fn single_trailing_junk_char_rejected() {
        let text = ":02000000ABCD86X\n:00000001FF\n";
        let err = parse_intel_hex(text).unwrap_err();
        assert!(err.to_string().contains("trailing bytes"), "got: {err}");
    }

    /* --- per-line decoding (new: parse_line in isolation) ------------- */

    #[test]
    fn parse_line_data_record() {
        let rec = HexRecord::parse_line(":04002000DEADBEEFA4").unwrap();
        assert_eq!(
            rec,
            HexRecord::Data {
                offset: 0x0020,
                bytes: vec![0xDE, 0xAD, 0xBE, 0xEF],
            }
        );
    }

    #[test]
    fn parse_line_eof_and_extended_records() {
        assert_eq!(
            HexRecord::parse_line(":00000001FF").unwrap(),
            HexRecord::Eof
        );
        assert_eq!(
            HexRecord::parse_line(":020000040001F9").unwrap(),
            HexRecord::ExtendedLinear(0x0001)
        );
        assert_eq!(
            HexRecord::parse_line(":020000021000EC").unwrap(),
            HexRecord::ExtendedSegment(0x1000)
        );
    }

    #[test]
    fn parse_line_ignores_start_address_records() {
        // Types 3 and 5 (start segment / start linear address) are emitted
        // by objcopy and carry no image data — accepted and ignored, but
        // still checksum-verified.
        for line in [":0400000300001234B3", ":04000005000021AC2A"] {
            let rec =
                HexRecord::parse_line(line).unwrap_or_else(|e| panic!("{line} should parse: {e}"));
            assert_eq!(rec, HexRecord::Ignored, "{line}");
        }
    }

    #[test]
    fn extended_record_payload_length_enforced() {
        // A 1-byte type-4 record would silently shift every later address.
        let err = HexRecord::parse_line(":0100000400FB").unwrap_err();
        assert!(
            err.to_string().contains("exactly 2 data bytes"),
            "got: {err}"
        );
    }

    /* --- accumulator-level guards (previously untested) --------------- */

    #[test]
    fn extended_segment_addressing_applies() {
        // Type 2 base is value << 4: 0x1000 << 4 = 0x10000.
        let text = ":020000021000EC\n:02000000ABCD86\n:00000001FF\n";
        let (base, img) = parse_intel_hex(text).unwrap();
        assert_eq!(base, 0x10000);
        assert_eq!(to_hex(&img), "abcd");
    }

    #[test]
    fn span_guard_rejects_oversized_image() {
        // Two bytes 32 MiB apart via linear base records.
        let text = ":02000000ABCD86\n:020000040200F8\n:02000000ABCD86\n:00000001FF\n";
        let err = parse_intel_hex(text).unwrap_err();
        assert!(err.to_string().contains("refusing"), "got: {err}");
    }

    #[test]
    fn address_overflow_guard_rejected() {
        // Linear base 0xFFFF puts the record's end past 2^32.
        let text = ":02000004FFFFFC\n:02FFFF00ABCD88\n:00000001FF\n";
        let err = parse_intel_hex(text).unwrap_err();
        assert!(
            err.to_string().contains("32-bit address space"),
            "got: {err}"
        );
    }
}
