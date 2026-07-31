//! OBP v0.1 frame codec: encode/decode with typed errors, response
//! matching, request payload builders, and status/detail decoding into
//! human-readable errors.
//!
//! A logical frame is identical on both transports:
//! `cmd(1) seq(1) len(1) flags(1) payload(0..56) crc32(4 LE)` with
//! CRC-32/ISO-HDLC over all preceding bytes. USB carries one frame per
//! zero-padded 64-byte HID report; UART prefixes `B0 07`.

pub mod consts;
pub mod device_info;

use std::fmt;

use anyhow::{bail, Result};

use consts::{
    OB_CMD_FRAME_ERR, OB_CMD_RESP_BIT, OB_DET_ADDR_ALIGN, OB_DET_ADDR_RANGE,
    OB_DET_VERIFY_MISMATCH, OB_DET_VERIFY_NONSEQ, OB_DET_VERIFY_NORECORD, OB_E_ADDR, OB_E_ARG,
    OB_E_CMD, OB_E_CRC, OB_E_FLASH, OB_E_LEN, OB_E_NOT_ERASED, OB_E_PROTO, OB_E_STATE, OB_E_VERIFY,
    OB_FRAME_HDR_LEN, OB_FRAME_OVERHEAD, OB_HELLO_MAGIC, OB_HELLO_REQ_LEN, OB_MAX_PAYLOAD, OB_OK,
};

/// One logical OBP frame (transport framing already removed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub cmd: u8,
    pub seq: u8,
    pub payload: Vec<u8>,
}

impl Frame {
    /// Payloads larger than `OB_MAX_PAYLOAD` are a host bug, not a runtime
    /// condition: every builder in this module stays well under the limit.
    pub fn new(cmd: u8, seq: u8, payload: Vec<u8>) -> Frame {
        assert!(
            payload.len() <= OB_MAX_PAYLOAD,
            "payload of {} bytes exceeds OB_MAX_PAYLOAD",
            payload.len()
        );
        Frame { cmd, seq, payload }
    }

    /// Total on-wire length: header + payload + CRC.
    pub fn wire_len(&self) -> usize {
        OB_FRAME_OVERHEAD + self.payload.len()
    }

    /// Encode to bytes: header + payload, then CRC-32/ISO-HDLC (LE) over
    /// everything before it. `flags` is always 0 in v0.1.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.wire_len());
        out.push(self.cmd);
        out.push(self.seq);
        out.push(self.payload.len() as u8);
        out.push(0); // flags: must be 0 in v0.1
        out.extend_from_slice(&self.payload);
        let crc = crc32fast::hash(&out);
        out.extend_from_slice(&crc.to_le_bytes());
        out
    }

    /// Decode and validate one frame from the start of `buf`. Bytes beyond
    /// the frame (HID report zero padding) are ignored.
    pub fn decode(buf: &[u8]) -> Result<Frame, FrameError> {
        if buf.len() < OB_FRAME_OVERHEAD {
            return Err(FrameError::TooShort { got: buf.len() });
        }
        let len = usize::from(buf[2]);
        if len > OB_MAX_PAYLOAD {
            return Err(FrameError::PayloadTooLong { declared: buf[2] });
        }
        let total = OB_FRAME_OVERHEAD + len;
        if buf.len() < total {
            return Err(FrameError::Truncated {
                declared: buf[2],
                available: buf.len(),
            });
        }
        if buf[3] != 0 {
            return Err(FrameError::BadFlags { flags: buf[3] });
        }
        let body_end = OB_FRAME_HDR_LEN + len;
        let computed = crc32fast::hash(&buf[..body_end]);
        let received = u32::from_le_bytes([
            buf[body_end],
            buf[body_end + 1],
            buf[body_end + 2],
            buf[body_end + 3],
        ]);
        if computed != received {
            return Err(FrameError::BadCrc { computed, received });
        }
        Ok(Frame {
            cmd: buf[0],
            seq: buf[1],
            payload: buf[OB_FRAME_HDR_LEN..body_end].to_vec(),
        })
    }

    /// `0xFF` frame-error report: the device could not parse a request
    /// (bad CRC/framing), so its echoed `seq` cannot be trusted.
    pub fn is_frame_error(&self) -> bool {
        self.cmd == OB_CMD_FRAME_ERR
    }

    /// Response matching: `cmd == req | 0x80` with the request's `seq`.
    pub fn is_response_to(&self, req: &Frame) -> bool {
        self.cmd == req.cmd | OB_CMD_RESP_BIT && self.seq == req.seq
    }
}

/// Typed frame decode/validation errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    /// Fewer bytes than the 8-byte minimum frame.
    TooShort { got: usize },
    /// `len` field exceeds `OB_MAX_PAYLOAD`.
    PayloadTooLong { declared: u8 },
    /// Buffer ends before `len` payload bytes + CRC.
    Truncated { declared: u8, available: usize },
    /// `flags` must be 0 in v0.1.
    BadFlags { flags: u8 },
    /// CRC over header+payload does not match the trailer.
    BadCrc { computed: u32, received: u32 },
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FrameError::TooShort { got } => {
                write!(f, "frame is {got} bytes; minimum is {OB_FRAME_OVERHEAD}")
            }
            FrameError::PayloadTooLong { declared } => {
                write!(
                    f,
                    "declared payload length {declared} exceeds {OB_MAX_PAYLOAD}"
                )
            }
            FrameError::Truncated {
                declared,
                available,
            } => write!(
                f,
                "frame truncated: {available} bytes available for a {declared}-byte payload"
            ),
            FrameError::BadFlags { flags } => {
                write!(f, "flags byte 0x{flags:02X} must be 0 in OBP v0.1")
            }
            FrameError::BadCrc { computed, received } => write!(
                f,
                "frame CRC mismatch: computed 0x{computed:08X}, received 0x{received:08X}"
            ),
        }
    }
}

impl std::error::Error for FrameError {}

/// A decoded non-OK status from a device response payload `[status, detail]`.
/// Status errors are definitive device answers and are never retried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceError {
    pub status: u8,
    pub detail: Option<u8>,
}

impl DeviceError {
    /// `None` when the payload is empty or carries `OB_OK`.
    pub fn from_payload(payload: &[u8]) -> Option<DeviceError> {
        match payload.split_first() {
            None | Some((&OB_OK, _)) => None,
            Some((&status, rest)) => Some(DeviceError {
                status,
                detail: rest.first().copied(),
            }),
        }
    }
}

impl fmt::Display for DeviceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "device reported: {}",
            describe_status(self.status, self.detail)
        )
    }
}

impl std::error::Error for DeviceError {}

/// Human-readable rendering of the unified status/detail taxonomy.
pub fn describe_status(status: u8, detail: Option<u8>) -> String {
    match status {
        OB_OK => "OK".to_string(),
        OB_E_CRC => "frame CRC mismatch (E_CRC)".to_string(),
        OB_E_LEN => "payload length invalid for this opcode (E_LEN)".to_string(),
        OB_E_CMD => "unknown opcode (E_CMD)".to_string(),
        OB_E_STATE => "command requires a session; send HELLO first (E_STATE)".to_string(),
        OB_E_ARG => "bad magic, mode, or flags (E_ARG)".to_string(),
        OB_E_ADDR => match detail {
            Some(OB_DET_ADDR_RANGE) => "address out of range (E_ADDR: range)".to_string(),
            Some(OB_DET_ADDR_ALIGN) => {
                "address or length misaligned (E_ADDR: alignment)".to_string()
            }
            _ => "address rejected (E_ADDR)".to_string(),
        },
        OB_E_NOT_ERASED => "write into a block not erased this session (E_NOT_ERASED)".to_string(),
        OB_E_FLASH => format!(
            "flash operation failed; ROM API returned 0x{:02X} (E_FLASH)",
            detail.unwrap_or(0)
        ),
        OB_E_VERIFY => match detail {
            Some(OB_DET_VERIFY_MISMATCH) => "image CRC mismatch (E_VERIFY: mismatch)".to_string(),
            Some(OB_DET_VERIFY_NONSEQ) => {
                "writes were not sequential from app_start (E_VERIFY: non-sequential)".to_string()
            }
            Some(OB_DET_VERIFY_NORECORD) => {
                "no valid boot record (E_VERIFY: no record)".to_string()
            }
            _ => "verification failed (E_VERIFY)".to_string(),
        },
        OB_E_PROTO => "unsupported protocol major version (E_PROTO)".to_string(),
        other => format!("unknown status 0x{other:02X}"),
    }
}

/* --- request payload builders ------------------------------------------ */

/// HELLO: magic "OBP1" (u32 LE) + host protocol major/minor.
pub fn hello_req_payload(host_major: u8, host_minor: u8) -> Vec<u8> {
    let mut p = Vec::with_capacity(OB_HELLO_REQ_LEN);
    p.extend_from_slice(&OB_HELLO_MAGIC.to_le_bytes());
    p.push(host_major);
    p.push(host_minor);
    p
}

/// ERASE: addr (u32 LE) + len (u32 LE), both block-aligned.
pub fn erase_req_payload(addr: u32, len: u32) -> Vec<u8> {
    let mut p = Vec::with_capacity(8);
    p.extend_from_slice(&addr.to_le_bytes());
    p.extend_from_slice(&len.to_le_bytes());
    p
}

/// WRITE: addr (u32 LE) + 4..48 bytes of 4-aligned data.
pub fn write_req_payload(addr: u32, data: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(4 + data.len());
    p.extend_from_slice(&addr.to_le_bytes());
    p.extend_from_slice(data);
    p
}

/// CRC: addr (u32 LE) + len (u32 LE).
pub fn crc_req_payload(addr: u32, len: u32) -> Vec<u8> {
    erase_req_payload(addr, len)
}

/// COMMIT: img_len (u32 LE) + img_crc32 (u32 LE).
pub fn commit_req_payload(img_len: u32, img_crc32: u32) -> Vec<u8> {
    let mut p = Vec::with_capacity(8);
    p.extend_from_slice(&img_len.to_le_bytes());
    p.extend_from_slice(&img_crc32.to_le_bytes());
    p
}

/// BOOT: mode byte (`OB_BOOT_APP` or `OB_BOOT_STAY`).
pub fn boot_req_payload(mode: u8) -> Vec<u8> {
    vec![mode]
}

/// Extract the CRC value from a successful CRC response payload
/// `[status, crc32 LE]`.
pub fn crc_resp_value(payload: &[u8]) -> Result<u32> {
    if payload.len() < 5 {
        bail!(
            "CRC response payload is {} bytes; expected status + 4-byte CRC",
            payload.len()
        );
    }
    Ok(u32::from_le_bytes([
        payload[1], payload[2], payload[3], payload[4],
    ]))
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::consts::*;
    use super::device_info::DeviceInfo;
    use super::*;

    /// Normative vectors: computed by protocol/gen_protocol.py, consumed
    /// here and by the firmware host-native tests.
    const GOLDEN: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../protocol/golden_frames.txt"
    ));

    fn unhex(s: &str) -> Vec<u8> {
        assert!(s.len().is_multiple_of(2), "odd-length hex: {s}");
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex byte"))
            .collect()
    }

    fn golden() -> BTreeMap<String, Vec<u8>> {
        let mut out = BTreeMap::new();
        for line in GOLDEN.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (name, hex) = line.split_once(':').expect("golden line is `name: hex`");
            out.insert(name.trim().to_string(), unhex(hex.trim()));
        }
        out
    }

    fn vector(name: &str) -> Vec<u8> {
        golden()
            .remove(name)
            .unwrap_or_else(|| panic!("missing golden vector {name}"))
    }

    /// Every vector in the file must be consumed by a test below; a new
    /// vector fails here until the codec tests cover it.
    #[test]
    fn golden_all_vectors_covered() {
        let names: BTreeSet<String> = golden().into_keys().collect();
        let covered: BTreeSet<String> = [
            "crc_check",
            "hello_req",
            "hello_resp_ch592_usb",
            "erase_req",
            "erase_ok",
            "write_req",
            "write_ok",
            "write_err_not_erased",
            "crc_req",
            "crc_ok",
            "commit_req",
            "commit_ok",
            "commit_err_nonseq",
            "boot_req",
            "boot_ok",
            "frame_err",
            "write_req_max",
            "min_frame",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            names, covered,
            "golden_frames.txt changed; update the codec tests to cover the new set"
        );
    }

    #[test]
    fn golden_crc_check() {
        let v = vector("crc_check");
        assert_eq!(crc32fast::hash(b"123456789").to_le_bytes().to_vec(), v);
        assert_eq!(crc32fast::hash(b"123456789"), 0xCBF43926);
    }

    #[test]
    fn golden_request_encodings() {
        assert_eq!(
            Frame::new(OB_CMD_HELLO, 0x00, hello_req_payload(0x00, 0x01)).encode(),
            vector("hello_req")
        );
        assert_eq!(
            Frame::new(OB_CMD_ERASE, 0x01, erase_req_payload(0x2000, 0x1000)).encode(),
            vector("erase_req")
        );
        assert_eq!(
            Frame::new(
                OB_CMD_WRITE,
                0x02,
                write_req_payload(0x2000, &unhex("deadbeefcafebabe"))
            )
            .encode(),
            vector("write_req")
        );
        assert_eq!(
            Frame::new(OB_CMD_CRC, 0x03, crc_req_payload(0x2000, 0x2000)).encode(),
            vector("crc_req")
        );
        assert_eq!(
            Frame::new(OB_CMD_COMMIT, 0x04, commit_req_payload(0x9C40, 0x12345678)).encode(),
            vector("commit_req")
        );
        assert_eq!(
            Frame::new(OB_CMD_BOOT, 0x05, boot_req_payload(OB_BOOT_APP)).encode(),
            vector("boot_req")
        );
        let data: Vec<u8> = (0..48).collect();
        assert_eq!(
            Frame::new(OB_CMD_WRITE, 0x06, write_req_payload(0x3000, &data)).encode(),
            vector("write_req_max")
        );
        assert_eq!(
            Frame::new(0x7F, 0xAA, Vec::new()).encode(),
            vector("min_frame")
        );
    }

    #[test]
    fn golden_hello_response_decodes() {
        let f = Frame::decode(&vector("hello_resp_ch592_usb")).unwrap();
        assert_eq!(f.cmd, OB_CMD_HELLO | OB_CMD_RESP_BIT);
        assert_eq!(f.seq, 0x00);
        assert_eq!(f.payload.len(), OB_HELLO_RESP_LEN);
        assert_eq!(f.payload[0], OB_OK);
        assert_eq!(DeviceError::from_payload(&f.payload), None);

        let info = DeviceInfo::parse(&f.payload).unwrap();
        assert_eq!(info.proto_major, 0);
        assert_eq!(info.proto_minor, 1);
        assert_eq!(info.chip_rev, 9);
        assert_eq!(info.bl_version, 0x000A);
        assert_eq!(info.chip_family, OB_FAMILY_CH592);
        assert_eq!(info.transport, OB_TRANSPORT_ID_USB);
        assert_eq!(info.app_start, 0x0000_2000);
        assert_eq!(info.app_end, 0x0007_0000);
        assert_eq!(info.erase_block, 4096);
        assert_eq!(info.write_page, 256);
        assert_eq!(info.write_align, 4);
        assert_eq!(info.max_write_data, 48);
        assert_eq!(info.features, OB_FEAT_CRC_LIVE);
        assert!(info.crc_live());
        assert_eq!(info.uid, 0x0123_4567_89AB_CDEF);
        assert_eq!(info.uid_hex(), "0123456789ABCDEF");
        assert_eq!(info.family_name(), "CH592");
        assert_eq!(info.transport_name(), "usb");
    }

    #[test]
    fn golden_status_responses_decode() {
        struct Case {
            name: &'static str,
            cmd: u8,
            seq: u8,
            error: Option<DeviceError>,
        }
        let cases = [
            Case {
                name: "erase_ok",
                cmd: 0x82,
                seq: 0x01,
                error: None,
            },
            Case {
                name: "write_ok",
                cmd: 0x83,
                seq: 0x02,
                error: None,
            },
            Case {
                name: "write_err_not_erased",
                cmd: 0x83,
                seq: 0x02,
                error: Some(DeviceError {
                    status: OB_E_NOT_ERASED,
                    detail: Some(OB_DET_NONE),
                }),
            },
            Case {
                name: "commit_ok",
                cmd: 0x85,
                seq: 0x04,
                error: None,
            },
            Case {
                name: "commit_err_nonseq",
                cmd: 0x85,
                seq: 0x04,
                error: Some(DeviceError {
                    status: OB_E_VERIFY,
                    detail: Some(OB_DET_VERIFY_NONSEQ),
                }),
            },
            Case {
                name: "boot_ok",
                cmd: 0x86,
                seq: 0x05,
                error: None,
            },
        ];
        for case in cases {
            let f = Frame::decode(&vector(case.name)).unwrap_or_else(|e| {
                panic!("{}: {e}", case.name);
            });
            assert_eq!(f.cmd, case.cmd, "{}", case.name);
            assert_eq!(f.seq, case.seq, "{}", case.name);
            assert!(!f.is_frame_error(), "{}", case.name);
            assert_eq!(
                DeviceError::from_payload(&f.payload),
                case.error,
                "{}",
                case.name
            );
        }
    }

    #[test]
    fn golden_crc_response() {
        let f = Frame::decode(&vector("crc_ok")).unwrap();
        assert_eq!(f.cmd, OB_CMD_CRC | OB_CMD_RESP_BIT);
        assert_eq!(f.seq, 0x03);
        assert_eq!(f.payload[0], OB_OK);
        assert_eq!(crc_resp_value(&f.payload).unwrap(), 0xCBF43926);
    }

    #[test]
    fn golden_frame_err_decodes_as_frame_error() {
        let f = Frame::decode(&vector("frame_err")).unwrap();
        assert!(f.is_frame_error());
        assert_eq!(f.cmd, OB_CMD_FRAME_ERR);
        assert_eq!(f.payload, vec![OB_E_CRC, OB_DET_NONE]);
        // A frame-error report is never a positive response to anything.
        let req = Frame::new(OB_CMD_HELLO, 0x09, hello_req_payload(0, 1));
        assert!(!f.is_response_to(&req));
        assert_eq!(
            DeviceError::from_payload(&f.payload),
            Some(DeviceError {
                status: OB_E_CRC,
                detail: Some(OB_DET_NONE)
            })
        );
    }

    #[test]
    fn golden_min_frame_roundtrips() {
        let bytes = vector("min_frame");
        let f = Frame::decode(&bytes).unwrap();
        assert_eq!(f, Frame::new(0x7F, 0xAA, Vec::new()));
        assert_eq!(f.encode(), bytes);
        assert_eq!(f.wire_len(), OB_FRAME_OVERHEAD);
    }

    #[test]
    fn decode_tolerates_hid_report_padding() {
        let mut report = vector("erase_ok");
        let frame = Frame::decode(&report).unwrap();
        report.resize(OB_MAX_FRAME, 0);
        assert_eq!(Frame::decode(&report).unwrap(), frame);
    }

    #[test]
    fn decode_typed_errors() {
        assert_eq!(
            Frame::decode(&[0x81, 0x00, 0x01]),
            Err(FrameError::TooShort { got: 3 })
        );

        let mut long = vector("min_frame");
        long[2] = 0x39; // 57 > OB_MAX_PAYLOAD
        assert_eq!(
            Frame::decode(&long),
            Err(FrameError::PayloadTooLong { declared: 0x39 })
        );

        let mut truncated = vector("erase_ok");
        truncated[2] = 0x10; // claims 16-byte payload, buffer has 1 + CRC
        assert_eq!(
            Frame::decode(&truncated),
            Err(FrameError::Truncated {
                declared: 0x10,
                available: 9
            })
        );

        let mut flagged = vector("min_frame");
        flagged[3] = 0x01;
        assert_eq!(
            Frame::decode(&flagged),
            Err(FrameError::BadFlags { flags: 1 })
        );

        let mut corrupt = vector("erase_ok");
        corrupt[4] ^= 0xFF; // flip the status byte; CRC no longer matches
        assert!(matches!(
            Frame::decode(&corrupt),
            Err(FrameError::BadCrc { .. })
        ));
    }

    #[test]
    fn encode_decode_roundtrip_all_payload_sizes() {
        for len in 0..=OB_MAX_PAYLOAD {
            let payload: Vec<u8> = (0..len as u8).collect();
            let f = Frame::new(0x42, len as u8, payload);
            assert_eq!(Frame::decode(&f.encode()).unwrap(), f);
        }
    }

    #[test]
    fn status_taxonomy_is_human_readable() {
        assert_eq!(describe_status(OB_OK, None), "OK");
        assert!(describe_status(OB_E_CRC, None).contains("CRC"));
        assert!(describe_status(OB_E_LEN, None).contains("length"));
        assert!(describe_status(OB_E_CMD, None).contains("opcode"));
        assert!(describe_status(OB_E_STATE, None).contains("HELLO"));
        assert!(describe_status(OB_E_ARG, None).contains("magic"));
        assert!(describe_status(OB_E_ADDR, Some(OB_DET_ADDR_RANGE)).contains("range"));
        assert!(describe_status(OB_E_ADDR, Some(OB_DET_ADDR_ALIGN)).contains("misaligned"));
        assert!(describe_status(OB_E_NOT_ERASED, None).contains("not erased"));
        assert!(describe_status(OB_E_FLASH, Some(0x2A)).contains("0x2A"));
        assert!(describe_status(OB_E_VERIFY, Some(OB_DET_VERIFY_MISMATCH)).contains("mismatch"));
        assert!(describe_status(OB_E_VERIFY, Some(OB_DET_VERIFY_NONSEQ)).contains("sequential"));
        assert!(describe_status(OB_E_VERIFY, Some(OB_DET_VERIFY_NORECORD)).contains("record"));
        assert!(describe_status(OB_E_PROTO, None).contains("protocol major"));
        assert!(describe_status(0x77, None).contains("0x77"));
    }
    /* --- generated constants match the header ------------------------- */

    const HEADER_SRC: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../protocol/openboot_protocol.h"
    ));
    const CONSTS_SRC: &str = include_str!("consts.rs");

    /// A C numeric literal: 0x-hex or decimal, one optional u/U suffix.
    fn parse_numeric(token: &str) -> Option<u64> {
        let t = token.trim_end_matches(['u', 'U']);
        if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
            u64::from_str_radix(hex, 16).ok()
        } else {
            t.parse::<u64>().ok()
        }
    }

    /// Tokens of a `#define` directive, or None if this line is not one.
    /// Mirrors `split_define()` in gen_protocol.py, including its tolerance
    /// for the whitespace C allows between `#` and `define` — the two
    /// parsers have to accept the same language or one of them becomes a
    /// blind spot the other cannot see.
    fn split_define(line: &str) -> Option<Vec<&str>> {
        let after_hash = line.trim_start().strip_prefix('#')?.trim_start();
        let rest = after_hash.strip_prefix("define")?;
        if !rest.starts_with([' ', '\t']) {
            return None; // `#defineOB_X` is not a define
        }
        Some(rest.split_whitespace().collect())
    }

    fn header_defines() -> BTreeMap<String, u64> {
        let mut out = BTreeMap::new();
        for line in HEADER_SRC.lines() {
            let Some(tokens) = split_define(line) else {
                continue;
            };
            let Some(name) = tokens.first() else { continue };
            if !name.starts_with("OB_") {
                continue;
            }
            let value = tokens
                .get(1)
                .copied()
                .and_then(parse_numeric)
                .unwrap_or_else(|| panic!("{name}: not a plain numeric literal"));
            // Anything past the value must be a comment: `1 << 8` would
            // otherwise read as 1 here AND in the generator, so both sides
            // would agree on a value the compiler never uses.
            if let Some(trailing) = tokens.get(2) {
                assert!(
                    trailing.starts_with("/*") || trailing.starts_with("//"),
                    "{name}: {:?} is not a single numeric literal",
                    tokens[1..].join(" ")
                );
            }
            assert!(
                out.insert((*name).to_string(), value).is_none(),
                "{name} twice"
            );
        }
        assert!(!out.is_empty(), "no OB_* defines found in the header");
        out
    }

    /// The generated `consts.rs` is checked against the header it claims to
    /// mirror, so a hand edit of the generated file (or a header change with
    /// no regeneration) fails here rather than at runtime on a device.
    #[test]
    fn generated_consts_match_header() {
        let mut generated = BTreeMap::new();
        for line in CONSTS_SRC.lines() {
            let Some(rest) = line.trim_start().strip_prefix("pub const ") else {
                continue;
            };
            let (name, tail) = rest.split_once(':').expect("const without a type");
            let value = tail
                .split_once('=')
                .and_then(|(_, v)| parse_numeric(v.trim().trim_end_matches(';')))
                .unwrap_or_else(|| panic!("{name}: unparsable generated value"));
            generated.insert(name.trim().to_string(), value);
        }

        let header = header_defines();
        for (name, value) in &header {
            match generated.get(name) {
                None => panic!("header defines {name} but consts.rs does not"),
                Some(got) => assert_eq!(got, value, "{name} disagrees"),
            }
        }
        for name in generated.keys() {
            assert!(header.contains_key(name), "consts.rs has stray {name}");
        }
        assert_eq!(header.len(), generated.len());
    }

    #[test]
    fn numeric_literal_parser() {
        assert_eq!(parse_numeric("0x38"), Some(0x38));
        assert_eq!(parse_numeric("115200"), Some(115200));
        assert_eq!(parse_numeric("0xB007CA11u"), Some(0xB007_CA11));
        assert_eq!(parse_numeric("(1<<2)"), None);
    }

    #[test]
    fn define_splitter_accepts_the_whitespace_c_does() {
        assert_eq!(split_define("#define OB_X 1"), Some(vec!["OB_X", "1"]));
        assert_eq!(split_define("  # define OB_X 1"), Some(vec!["OB_X", "1"]));
        assert_eq!(split_define("#\tdefine OB_X 1"), Some(vec!["OB_X", "1"]));
        assert_eq!(
            split_define("#define OB_X 0x1u  /* note */"),
            Some(vec!["OB_X", "0x1u", "/*", "note", "*/"])
        );
        assert_eq!(split_define("#defineOB_X 1"), None);
        assert_eq!(split_define("#include <x.h>"), None);
        assert_eq!(split_define("int x = 1;"), None);
    }
}
