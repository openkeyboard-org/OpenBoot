/*
 * OpenBoot wire protocol ("OBP") v0.1 — single source of truth.
 *
 * This header is the hand-written source of truth. It is included directly
 * by the bootloader firmware, and protocol/gen_protocol.py parses its
 * numeric OB_* defines to GENERATE tools/src/proto/consts.rs,
 * firmware/tests/ob_consts.py, and the golden wire vectors. Change a value
 * here, then run the generator; `make -C firmware test` (--check) and a
 * cargo test both fail while anything is stale.
 *
 * Frame (identical logical bytes on USB and UART):
 *
 *   offset  size  field
 *   0       1     cmd      request 0x01..0x7F; response = request | 0x80;
 *                          0xFF = frame-error report
 *   1       1     seq      opaque, chosen by host, echoed by device
 *   2       1     len      payload length N (0..OB_MAX_PAYLOAD)
 *   3       1     flags    must be 0 in v0.1 (rejected with E_ARG otherwise)
 *   4       N     payload
 *   4+N     4     crc32    CRC-32/ISO-HDLC (zlib), little-endian, over
 *                          bytes [0, 4+N)
 *
 * USB: one frame per 64-byte HID report, zero-padded, both directions.
 * UART: 0xB0 0x07 start-of-frame prefix, then the frame bytes; no trailer.
 *
 * All multi-byte integers are little-endian.
 */
#ifndef OPENBOOT_PROTOCOL_H
#define OPENBOOT_PROTOCOL_H

#include <stdint.h>

/* --- protocol version ------------------------------------------------- */
/* Semver semantics: while OB_PROTO_MAJOR == 0 nothing is frozen and any
 * minor bump may break compatibility, so HELLO requires an EXACT
 * major+minor match. From 1.0 on, majors gate compatibility and minors
 * are additive. (The "OBP1" HELLO magic is the protocol FAMILY
 * identifier, not the version — these two bytes are the version.) */
#define OB_PROTO_MAJOR        0x00
#define OB_PROTO_MINOR        0x01

/* --- frame geometry --------------------------------------------------- */
#define OB_FRAME_HDR_LEN      0x04
#define OB_FRAME_CRC_LEN      0x04
#define OB_FRAME_OVERHEAD     0x08
#define OB_MAX_PAYLOAD        0x38  /* 56 */
#define OB_MAX_FRAME          0x40  /* 64 = one HID report */
#define OB_MAX_WRITE_DATA     0x30  /* 48: multiple of 4 and 16 */

/* --- UART transport --------------------------------------------------- */
#define OB_UART_SOF1          0xB0
#define OB_UART_SOF2          0x07
#define OB_UART_BAUD          115200
#define OB_UART_INTERBYTE_MS  0x32  /* 50: mid-frame gap that resets the RX parser */

/* --- commands ---------------------------------------------------------- */
#define OB_CMD_HELLO          0x01
#define OB_CMD_ERASE          0x02
#define OB_CMD_WRITE          0x03
#define OB_CMD_CRC            0x04
#define OB_CMD_COMMIT         0x05
#define OB_CMD_BOOT           0x06
#define OB_CMD_READ           0x07  /* reserved; only with OB_FEAT_READ */
#define OB_CMD_RESP_BIT       0x80
#define OB_CMD_FRAME_ERR      0xFF

/* --- status codes (error responses carry payload [status, detail]) ----- */
#define OB_OK                 0x00
#define OB_E_CRC              0x01  /* frame CRC mismatch (via 0xFF report) */
#define OB_E_LEN              0x02  /* payload length invalid for opcode    */
#define OB_E_CMD              0x03  /* unknown opcode                       */
#define OB_E_STATE            0x04  /* command needs a session (no HELLO)   */
#define OB_E_ARG              0x05  /* bad magic / mode / flags             */
#define OB_E_ADDR             0x06  /* detail: OB_DET_ADDR_*                */
#define OB_E_NOT_ERASED       0x07  /* WRITE into a block not erased this session */
#define OB_E_FLASH            0x08  /* detail: low byte of ROM API return   */
#define OB_E_VERIFY           0x09  /* detail: OB_DET_VERIFY_*              */
#define OB_E_PROTO            0x0A  /* unsupported protocol major in HELLO  */

#define OB_DET_NONE           0x00
#define OB_DET_ADDR_RANGE     0x01
#define OB_DET_ADDR_ALIGN     0x02
#define OB_DET_VERIFY_MISMATCH 0x01
#define OB_DET_VERIFY_NONSEQ  0x02
#define OB_DET_VERIFY_NORECORD 0x03

/* --- HELLO ------------------------------------------------------------- */
/* request payload: magic "OBP1" (u32 LE), host_major u8, host_minor u8    */
#define OB_HELLO_MAGIC        0x3150424Fu  /* "OBP1" read as u32 LE */
#define OB_HELLO_REQ_LEN      0x06
/* response payload (device must send >= this; host must tolerate more):
 *   0  status  1 proto_major  2 proto_minor  3 chip_rev  4 bl_version u16
 *   6  chip_family  7 transport  8 app_start u32  12 app_end u32
 *   16 erase_block u32  20 write_page u16  22 write_align u8
 *   23 max_write_data u8  24 features u32  28 uid u64                     */
#define OB_HELLO_RESP_LEN     0x24  /* 36 */

#define OB_FAMILY_CH570       0x01
#define OB_FAMILY_CH572       0x02
#define OB_FAMILY_CH591       0x03
#define OB_FAMILY_CH592       0x04

#define OB_TRANSPORT_ID_USB   0x01
#define OB_TRANSPORT_ID_UART  0x02

#define OB_FEAT_READ          0x01  /* bit0: READ command available          */
#define OB_FEAT_CRC_LIVE      0x02  /* bit1: CRC is authoritative for flash
                                     * written this power cycle (clear on
                                     * CH57x: XIP may serve stale data)      */

/* --- BOOT modes -------------------------------------------------------- */
#define OB_BOOT_APP           0x00
#define OB_BOOT_STAY          0x01

/* --- boot record (16 bytes, stored by the bootloader at COMMIT) --------- */
/* CH57x: code flash 0x3B000; CH59x: DataFlash offset 0x7000 (last block). */
#define OB_RECORD_MAGIC       0x3152424Fu  /* "OBR1" read as u32 LE */

typedef struct {
    uint32_t magic;      /* OB_RECORD_MAGIC */
    uint32_t img_len;    /* bytes from app_start, multiple of 4 */
    uint32_t img_crc32;  /* CRC-32/ISO-HDLC over [app_start, app_start+img_len) */
    uint32_t rec_crc32;  /* CRC-32/ISO-HDLC over the 12 bytes above */
} ob_boot_record_t;

/* --- app -> bootloader entry request ------------------------------------ */
/* The application writes OB_BOOTREQ_MAGIC to the reserved top-of-RAM word
 * and performs a software reset; the bootloader checks and clears it. The
 * top 16 bytes of RAM are reserved (kept outside the stack by both the
 * bootloader's and the app companion's linker guidance). */
#define OB_BOOTREQ_MAGIC      0xB007CA11u
#define OB_BOOTREQ_ADDR_CH57X 0x20002FF0u  /* 12 K RAM */
#define OB_BOOTREQ_ADDR_CH59X 0x200067F0u  /* 26 K RAM */

/* --- shared layout facts ------------------------------------------------ */
/* The bootloader owns [0, OB_APP_BASE). The 8 KiB region is uniform across
 * chips and transports; USB HID images do not fit in 4 KiB. */
#define OB_APP_BASE           0x00002000u  /* all supported chips */

#endif /* OPENBOOT_PROTOCOL_H */
