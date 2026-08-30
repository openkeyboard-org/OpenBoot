/*
 * Open flash driver for the CH5xx families: the replacement for the
 * binary-only libISP archives (libISP572.a / libISP592.a).
 *
 * Behavioral reference is the disassembly of those archives (ISP572.o,
 * ISP583.o), cross-checked against ch32fun's independently reverse-engineered
 * ch5xx_flash.h (MIT). The controller is a byte-serial front end to an
 * internal SPI-NOR die; the registers and RB_ROM_* gate bits are the vendor's
 * own names from CH572SFR.h / CH592SFR.h.
 */
#ifndef OPENBOOT_FLASH_CH5XX_H
#define OPENBOOT_FLASH_CH5XX_H

#include <stdint.h>

/* Failure codes. The low byte of a nonzero return travels on the wire as the
 * E_FLASH detail (docs/PROTOCOL.md), where detail 0x00 is reserved for the
 * core's generation-ceiling response — so every code here has a nonzero low
 * byte. High nibble = operation, low nibble = cause; the values are driver
 * diagnostics, not a stable API. */
#define OB_FLERR_ERASE_PARAM     0x51u
#define OB_FLERR_ERASE_TIMEOUT   0x52u
#define OB_FLERR_WRITE_PARAM     0x61u
#define OB_FLERR_WRITE_TIMEOUT   0x62u
#define OB_FLERR_VERIFY_PARAM    0x71u
#define OB_FLERR_VERIFY_MISMATCH 0x72u

/* Code-flash primitives. Return 0 on success, an OB_FLERR_* code otherwise.
 * buf must be in RAM and 4-byte aligned; len a multiple of 4 (erase: of
 * 4096). No range checks: the core validates against the app region first. */
uint32_t ob_ch5xx_flash_erase(uint32_t addr, uint32_t len);
uint32_t ob_ch5xx_flash_write(uint32_t addr, const void *buf, uint32_t len);
uint32_t ob_ch5xx_flash_verify(uint32_t addr, const void *buf, uint32_t len);

/* Identity reads for ob_family_read_uid(). uid: 16 bytes clocked from opcode
 * 0x4B, XOR-folded into buf[0..7] (buf[8..15] untouched). rom_info: 8 bytes
 * from `addr` in the info window; stores one word at buf+0 and, when addr
 * bit 13 is set (the MAC window), a halfword at buf+4, else a word at buf+4
 * — byte-identical layout to the vendor archive. */
void ob_ch5xx_flash_uid_read(uint32_t buf[4]);
void ob_ch5xx_flash_rom_info_read(uint32_t addr, uint32_t buf[4]);

#endif /* OPENBOOT_FLASH_CH5XX_H */
