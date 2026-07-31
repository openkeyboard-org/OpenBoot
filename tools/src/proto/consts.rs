//! Protocol constants — GENERATED, DO NOT EDIT.
//!
//! Source of truth: `protocol/openboot_protocol.h`.
//! Regenerate with `python3 protocol/gen_protocol.py`; the
//! `generated_consts_match_header` test fails if this file and the header
//! ever disagree, so a hand edit is caught rather than trusted.

// Every header constant is emitted whether or not the tool uses it yet.
#![allow(dead_code)]

pub const OB_PROTO_MAJOR: u8 = 0x0;
pub const OB_PROTO_MINOR: u8 = 0x1;
pub const OB_FRAME_HDR_LEN: usize = 0x4;
pub const OB_FRAME_CRC_LEN: usize = 0x4;
pub const OB_FRAME_OVERHEAD: usize = 0x8;
pub const OB_MAX_PAYLOAD: usize = 0x38;
pub const OB_MAX_FRAME: usize = 0x40;
pub const OB_MAX_WRITE_DATA: usize = 0x30;
pub const OB_UART_SOF1: u8 = 0xB0;
pub const OB_UART_SOF2: u8 = 0x7;
pub const OB_UART_BAUD: u32 = 0x1C200;
pub const OB_UART_INTERBYTE_MS: u64 = 0x32;
pub const OB_CMD_HELLO: u8 = 0x1;
pub const OB_CMD_ERASE: u8 = 0x2;
pub const OB_CMD_WRITE: u8 = 0x3;
pub const OB_CMD_CRC: u8 = 0x4;
pub const OB_CMD_COMMIT: u8 = 0x5;
pub const OB_CMD_BOOT: u8 = 0x6;
pub const OB_CMD_READ: u8 = 0x7;
pub const OB_CMD_RESP_BIT: u8 = 0x80;
pub const OB_CMD_FRAME_ERR: u8 = 0xFF;
pub const OB_OK: u8 = 0x0;
pub const OB_E_CRC: u8 = 0x1;
pub const OB_E_LEN: u8 = 0x2;
pub const OB_E_CMD: u8 = 0x3;
pub const OB_E_STATE: u8 = 0x4;
pub const OB_E_ARG: u8 = 0x5;
pub const OB_E_ADDR: u8 = 0x6;
pub const OB_E_NOT_ERASED: u8 = 0x7;
pub const OB_E_FLASH: u8 = 0x8;
pub const OB_E_VERIFY: u8 = 0x9;
pub const OB_E_PROTO: u8 = 0xA;
pub const OB_DET_NONE: u8 = 0x0;
pub const OB_DET_ADDR_RANGE: u8 = 0x1;
pub const OB_DET_ADDR_ALIGN: u8 = 0x2;
pub const OB_DET_VERIFY_MISMATCH: u8 = 0x1;
pub const OB_DET_VERIFY_NONSEQ: u8 = 0x2;
pub const OB_DET_VERIFY_NORECORD: u8 = 0x3;
pub const OB_HELLO_MAGIC: u32 = 0x3150424F;
pub const OB_HELLO_REQ_LEN: usize = 0x6;
pub const OB_HELLO_RESP_LEN: usize = 0x24;
pub const OB_FAMILY_CH570: u8 = 0x1;
pub const OB_FAMILY_CH572: u8 = 0x2;
pub const OB_FAMILY_CH591: u8 = 0x3;
pub const OB_FAMILY_CH592: u8 = 0x4;
pub const OB_TRANSPORT_ID_USB: u8 = 0x1;
pub const OB_TRANSPORT_ID_UART: u8 = 0x2;
pub const OB_FEAT_READ: u32 = 0x1;
pub const OB_FEAT_CRC_LIVE: u32 = 0x2;
pub const OB_BOOT_APP: u8 = 0x0;
pub const OB_BOOT_STAY: u8 = 0x1;
pub const OB_RECORD_MAGIC: u32 = 0x3152424F;
pub const OB_BOOTREQ_MAGIC: u32 = 0xB007CA11;
pub const OB_BOOTREQ_ADDR_CH57X: u32 = 0x20002FF0;
pub const OB_BOOTREQ_ADDR_CH59X: u32 = 0x200067F0;
pub const OB_APP_BASE: u32 = 0x2000;
