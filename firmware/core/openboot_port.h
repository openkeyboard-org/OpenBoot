/*
 * Port contract: everything the portable core needs from a chip family.
 *
 * A port is firmware/ports/<family>/port_<family>.{h,c}. The port header is
 * included via the build system (-include or PORT_HEADER define); it must
 * provide the constants checked below. Shared CH5xx hooks live in
 * ports/port_ch5xx.c and family-specific hooks in the family port .c file.
 * The core includes NO SDK headers — SFR access is confined to ports and
 * transports.
 */
#ifndef OPENBOOT_PORT_H
#define OPENBOOT_PORT_H

#include <stdint.h>
#include "../../protocol/openboot_protocol.h"

/* ---- constants a port must define ------------------------------------ */
/* OB_FLASH_APP_START   app base (0x2000 everywhere; see OB_APP_BASE)
 * OB_FLASH_APP_END     exclusive app end — supplied per chip VARIANT by the
 *                      Makefile's generated openboot_config.h:
 *                      ch570/ch572 0x3C000, ch591 0x30000, ch592 0x70000
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

/* ---- A/B slot geometry (injected by the Makefile) --------------------- */
/* Two equally sized slots inside the app region. They are equal because the
 * application is LINKED per slot rather than copied between them — see the
 * geometry block in firmware/Makefile for why a copy is not available on
 * these parts. Any region remainder is left unused above slot B. */
#ifndef OB_SLOT_SIZE
#error "build must inject OB_SLOT_SIZE"
#endif
#ifndef OB_SLOT_A_BASE
#error "build must inject OB_SLOT_A_BASE"
#endif
#ifndef OB_SLOT_B_BASE
#error "build must inject OB_SLOT_B_BASE"
#endif
#if (OB_FLASH_APP_START % OB_FLASH_ERASE_BLOCK) != 0
#error "app base must be erase-block aligned, or a slot record lands mid-block"
#endif
#if (OB_SLOT_SIZE % OB_FLASH_ERASE_BLOCK) != 0
#error "slot size must be a multiple of the erase block"
#endif
/* Strictly greater, not merely non-zero: the top block of every slot belongs
 * to its record, so a one-block slot has no room for an image at all. That
 * configuration builds, and every ERASE and WRITE then fails the range check
 * on a capacity of zero — a bootloader that cannot be flashed. It is
 * reachable from a board setting OB_APP_END barely above the app base. */
#if OB_SLOT_SIZE <= OB_FLASH_ERASE_BLOCK
#error "slot size must exceed one erase block: the record owns the top block"
#endif
#if OB_SLOT_A_BASE != OB_FLASH_APP_START
#error "slot A must start at the app base"
#endif
#if OB_SLOT_B_BASE != (OB_SLOT_A_BASE + OB_SLOT_SIZE)
#error "slot B must start immediately after slot A"
#endif
#if (OB_SLOT_B_BASE + OB_SLOT_SIZE) > OB_FLASH_APP_END
#error "slot B must end inside the app region"
#endif

/* Erased-block bitmap: one bit per block in the one slot this session may
 * mutate. Deriving it from slot geometry avoids a chip-specific maximum. */
#define OB_BITMAP_BYTES \
    ((OB_SLOT_SIZE / OB_FLASH_ERASE_BLOCK + 7u) / 8u)
#if OB_FLASH_APP_END <= OB_FLASH_APP_START
#error "application region must be non-empty"
#endif

/* ---- hooks supplied by the shared and family port sources ------------ */

/* Clock / pin bring-up. UART builds keep the reset clock. USB builds use
 * the family PLL setting. Called once before transport init. */
void ob_port_init(void);

/* Thin wrappers over FLASH_EEPROM_CMD. Return 0 on success (ROM
 * convention). buf must be in RAM, 4-byte aligned; len multiple of 4.
 * These do NOT range-check — the core validates against the app region
 * before calling (the flash driver would happily write anywhere). */
uint32_t ob_flash_erase(uint32_t addr, uint32_t len);
uint32_t ob_flash_write(uint32_t addr, const void *buf, uint32_t len);
uint32_t ob_flash_verify(uint32_t addr, const void *buf, uint32_t len);

/* No boot-record hooks: each slot's record lives INSIDE that slot, in code
 * flash on both families, so it is read through XIP and written with the
 * flash hooks above like any other app byte. See docs/AB-UPDATE.md. */

/* Boot strap pin, debounced by the caller. Returns nonzero when the
 * stay-in-bootloader strap is asserted. Compiled to `return 0` when the
 * board disables it (OB_BOOT_PIN_MASK undefined). */
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

/* Free-running milliseconds since ob_port_init(). Monotonic, wraps at 2^32
 * (~49 days). Backed by SysTick, which the port starts once the clock is
 * settled — this is the only real time source in the bootloader.
 *
 * CONTRACT: the caller must poll this more often than the underlying tick
 * counter wraps, which is ~43 s at the fastest supported clock. The main
 * loop calls it every iteration (tens of microseconds), so the accumulator
 * cannot miss a wrap. Anything that blocks for tens of seconds without
 * calling it would lose time. */
uint32_t ob_uptime_ms(void);

/* Launch the application at `base` — the base of the slot the boot decision
 * chose, not a fixed address, because either slot may be active.
 *
 * CONTRACT: machine state the port owes the application at entry —
 * interrupts globally off (MIE=0) with no interrupt-controller enable or
 * pending bit armed by the bootloader; SysTick in its reset state (stopped,
 * count flag cleared, counter zeroed — CMP excepted, see port_ch5xx.c);
 * the trap vector still on the bootloader's trap spin until the app
 * installs its own, exactly as on a cold boot; the system clock as the
 * build configured it (USB: the family PLL; UART: the boot clock, or the
 * board's OB_CPU_HZ override); transport hardware quiesced per
 * tr_deinit(). Anything else the bootloader touches must either be returned
 * to reset state before the jump or documented in app/README.md. */
void ob_jump_app(uint32_t base) __attribute__((noreturn));

/* Soft-reset the chip. Where the next boot lands is not this function's
 * business — ob_boot_decide() re-runs and picks. BOOT uses it to LAUNCH
 * the app (reset first, so the CH57x XIP view is coherent) and to stay,
 * the difference being only whether the boot-request magic is armed. */
void ob_reset(void) __attribute__((noreturn));

#endif /* OPENBOOT_PORT_H */
