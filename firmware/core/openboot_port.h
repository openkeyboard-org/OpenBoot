/*
 * Port contract: everything the portable core needs from a chip family.
 *
 * A port is firmware/ports/<family>/port_<family>.{h,c}. The port header is
 * included via the build system (-include or PORT_HEADER define); it must
 * provide the constants checked below and the hook implementations live in
 * the port .c file. The core includes NO SDK headers — SFR access is
 * confined to ports and transports.
 */
#ifndef OPENBOOT_PORT_H
#define OPENBOOT_PORT_H

#include <stdint.h>
#include "../../protocol/openboot_protocol.h"

/* ---- constants a port must define ------------------------------------ */
/* OB_FLASH_APP_START   app base (0x2000 everywhere; see OB_APP_BASE)
 * OB_FLASH_APP_END     exclusive app end — supplied per chip VARIANT by the
 *                      Makefile's generated openboot_config.h:
 *                      ch570/ch572 0x3B000, ch591 0x30000, ch592 0x70000
 * OB_FLASH_ERASE_BLOCK 4096
 * OB_FLASH_WRITE_PAGE  256
 * OB_ERASED_WORD       XIP value of an erased word (0xF3F9BDA9, both families)
 * OB_CPU_HZ            6400000 (UART), 100000000 (ch57x USB), or
 *                      60000000 (ch59x USB)
 * OB_CHIP_FAMILY       OB_FAMILY_* — injected per VARIANT by the Makefile
 * OB_BOOTREQ_ADDR      OB_BOOTREQ_ADDR_CH57X / _CH59X
 * OB_FEATURES          OB_FEAT_* bits (CH59x sets OB_FEAT_CRC_LIVE)
 */
#include OB_PORT_HEADER   /* from the generated openboot_config.h */

#ifndef OB_FLASH_APP_START
#error "port must define OB_FLASH_APP_START"
#endif
#ifndef OB_FLASH_APP_END
#error "build must inject OB_FLASH_APP_END"
#endif
#ifndef OB_FLASH_ERASE_BLOCK
#error "port must define OB_FLASH_ERASE_BLOCK"
#endif
#ifndef OB_FLASH_WRITE_PAGE
#error "port must define OB_FLASH_WRITE_PAGE"
#endif
#ifndef OB_ERASED_WORD
#error "port must define OB_ERASED_WORD"
#endif
#ifndef OB_CPU_HZ
#error "build must inject OB_CPU_HZ"
#endif
#ifndef OB_CHIP_FAMILY
#error "build must inject OB_CHIP_FAMILY"
#endif
#ifndef OB_BOOTREQ_ADDR
#error "port must define OB_BOOTREQ_ADDR"
#endif
#ifndef OB_FEATURES
#error "port must define OB_FEATURES"
#endif

/* Erased-block bitmap: 1 bit per erase block of the largest app region
 * (CH592: 440 KiB / 4 KiB = 110 blocks). */
#define OB_BITMAP_BYTES 16
#if (((OB_FLASH_APP_END - OB_FLASH_APP_START + OB_FLASH_ERASE_BLOCK - 1u) / \
      OB_FLASH_ERASE_BLOCK) > (OB_BITMAP_BYTES * 8u))
#error "application erase-block count exceeds OB_BITMAP_BYTES capacity"
#endif

/* ---- hooks a port must implement (port_<family>.c) ------------------- */

/* Clock / pin bring-up. UART builds keep the reset clock. USB builds use
 * the family PLL setting. Called once before transport init. */
void ob_port_init(void);

/* Thin wrappers over FLASH_EEPROM_CMD. Return 0 on success (ROM
 * convention). buf must be in RAM, 4-byte aligned; len multiple of 4.
 * These do NOT range-check — the core validates against the app region
 * before calling (the ROM API would happily write anywhere). */
uint32_t ob_flash_erase(uint32_t addr, uint32_t len);
uint32_t ob_flash_write(uint32_t addr, const void *buf, uint32_t len);
uint32_t ob_flash_verify(uint32_t addr, const void *buf, uint32_t len);

/* Boot-record storage (CH57x: code-flash page; CH59x: DataFlash last
 * block — always erased with a full 4096-byte length, never the SDK
 * EEPROM_ERASE inline). read returns 0 and fills *rec on success. */
int      ob_record_read(ob_boot_record_t *rec);
uint32_t ob_record_write(const ob_boot_record_t *rec);
uint32_t ob_record_invalidate(void);

/* Boot strap pin, debounced by the caller. Returns nonzero when the
 * stay-in-bootloader strap is asserted. Compiled to `return 0` when the
 * board disables it (OB_BOOT_PIN undefined). */
int ob_bootpin_asserted(void);

/* Chip identity for HELLO. */
uint8_t ob_chip_rev(void);          /* ROM config chip-id byte */

/* Exclusive end of the app region, CLAMPED to what this silicon actually
 * has. OB_FLASH_APP_END is a build-time constant, so a wrong-variant image
 * would otherwise advertise and accept a region the die does not have (a
 * ch592 image on a CH591 claims 448 KiB on a 192 KiB part). The chip-id byte
 * is the only runtime evidence of what we are really running on, so every
 * range decision goes through here rather than through the constant.
 * An unrecognised id returns the compiled value: refusing to run would
 * strand a user on a new variant, and the host cross-checks the id anyway. */
uint32_t ob_app_end(void);
void    ob_read_uid(uint8_t uid[8]);

void ob_delay_us(uint32_t us);

void ob_jump_app(void) __attribute__((noreturn));

/* Soft-reset the chip. Where the next boot lands is not this function's
 * business — ob_boot_decide() re-runs and picks. BOOT uses it to LAUNCH
 * the app (reset first, so the CH57x XIP view is coherent) and to stay,
 * the difference being only whether the boot-request magic is armed. */
void ob_reset(void) __attribute__((noreturn));

#endif /* OPENBOOT_PORT_H */
