/*
 * Open flash driver for CH57x/CH59x: byte-serial SPI-NOR controller.
 *
 * Every sequence below reproduces the disassembled vendor archives
 * (libISP572.a ISP572.o, libISP592.a ISP583.o) instruction-for-instruction in
 * effect; where the two archives disagree the difference is a per-family
 * OB_FL_* constant with the divergence noted at its definition. ch32fun's
 * ch5xx_flash.h (MIT, independently reverse-engineered) was used as a
 * cross-check only — its verify clocking and its GLOB_ROM_CFG restore differ
 * from the archives and are NOT followed.
 *
 * Execution contract: everything here runs from RAM (.highcode.*) because
 * XIP is unavailable from R8_FLASH_CTRL=5 until the transaction ends, and
 * nothing may call back into flash .text in between — so this file is built
 * with -mno-save-restore -fno-jump-tables and keeps no string literals or
 * const tables (see check_highcode.py). IRQs need no masking: the bootloader
 * runs with MIE=0 for its entire life (startup_ch5xx.S), unlike the vendor
 * archive, which masks the PFIC around every operation.
 */
#include "openboot_port.h"
#include "flash_ch5xx.h"

#ifndef OB_FL_HIGHCODE
#define OB_FL_HIGHCODE(fn) __attribute__((section(".highcode." fn)))
#endif

/* Register access seam: the host unit tests substitute a recording mock by
 * defining OB_FL_REGS_MOCKED (and these macros) in their port header — same
 * pattern as the OB_XIP_READ* overrides in core/ob_xip.h. */
#ifndef OB_FL_REGS_MOCKED
#define OB_FL_CTRL_RD()    R8_FLASH_CTRL
#define OB_FL_CTRL_WR(v)   (R8_FLASH_CTRL = (uint8_t)(v))
#define OB_FL_DATA8_RD()   R8_FLASH_DATA
#define OB_FL_DATA8_WR(v)  (R8_FLASH_DATA = (uint8_t)(v))
#define OB_FL_DATA32_RD()  R32_FLASH_DATA
#define OB_FL_DATA32_WR(v) (R32_FLASH_DATA = (uint32_t)(v))
#define OB_FL_GLOB_RD()    R8_GLOB_ROM_CFG
#define OB_FL_GLOB_WR(v)   (R8_GLOB_ROM_CFG = (uint8_t)(v))
#define OB_FL_NOP()        __asm__ volatile ("nop")
#endif

#if (OB_CHIP_FAMILY == OB_FAMILY_CH570) || (OB_CHIP_FAMILY == OB_FAMILY_CH572)
/* ISP572: FLASH_START ORs the full write gate for EVERY operation, reads
 * included; FLASH_ROM_BEG has two nops; the 0xFF resume is FLASH_ROM_BEG_FF,
 * which sends the byte twice with a busy-wait between; the info window is
 * addressed raw. RB_ROM_CODE_WE is the two-bit 0xC0 field on this family. */
#define OB_FL_GATE_WRITE   (RB_ROM_CTRL_EN | RB_ROM_CODE_WE)          /* 0xE0 */
#define OB_FL_GATE_READ    OB_FL_GATE_WRITE
#define OB_FL_BEGIN_NOPS   2
#define OB_FL_RESUME_TWICE 1
#define OB_FL_INFO_OR      0u
#if RB_ROM_CODE_WE != 0xC0
#error "RB_ROM_CODE_WE drifted from the 0xC0 the archive's 0xE0 gate encodes"
#endif
#else
/* ISP583: the dispatcher picks 0xE0 only for erase/write commands and plain
 * RB_ROM_CTRL_EN for reads; FLASH_ROM_BEG has one nop; the 0xFF resume is a
 * single send; info-window reads OR 0x80000 into the address. */
#define OB_FL_GATE_WRITE   (RB_ROM_CTRL_EN | RB_ROM_DATA_WE | RB_ROM_CODE_WE)
#define OB_FL_GATE_READ    RB_ROM_CTRL_EN                             /* 0x20 */
#define OB_FL_BEGIN_NOPS   1
#define OB_FL_RESUME_TWICE 0
#define OB_FL_INFO_OR      0x80000u
#if (RB_ROM_CODE_WE != 0x80) || (RB_ROM_DATA_WE != 0x40)
#error "RB_ROM_*_WE drifted from the bits the archive's 0xE0 gate encodes"
#endif
#endif

/* The disassembled archives' gates are literal 0xE0 / 0x20 writes; the
 * driver composes them from the SFR names, so pin the composition. This
 * also keeps the host mock's hand-copied constants honest — it fails to
 * compile against a drifted SDK header the same way firmware would. */
#if (OB_FL_GATE_WRITE != 0xE0) || (RB_ROM_CTRL_EN != 0x20) || \
    (RB_ROM_CODE_OFS != 0x10)
#error "flash gate composition no longer matches the vendor archives"
#endif

/* WIP poll bound. The archives spin a fixed 0x80000 iterations regardless of
 * sysclk, so their wall-clock bound swings with the CPU clock (~0.3 s at
 * 100 MHz to ~5 s at the 6.4 MHz every UART image runs). This bound instead
 * targets a fixed wall time: one poll iteration is a full status transaction
 * — at least ~64 CPU cycles plus the SCK-domain byte transfers — so
 * OB_CPU_HZ / 40 gives roughly 1.6 s or more at every supported clock. That
 * is >= 4x a 400 ms worst-case SPI-NOR sector erase (bench-measured CH592:
 * ~17.5 ms typical, ~90x margin) and shorter than the archives' bound only
 * in the 6.4 MHz regime, where it still holds the 4x worst-case margin. A
 * wedged die is fatal either way; the bound errs toward never declaring a
 * live one dead. */
#define OB_FL_WAIT_ITERS   (OB_CPU_HZ / 40u)

/* Erase opcode and step derive from OB_FLASH_ERASE_BLOCK — the core's single
 * granularity source, set by the OB_FLASH_PAGE_ERASE knob in the port header —
 * so the driver's step can never drift from the block size the core erases one
 * at a time. 4096 -> sector erase 0x20; 256 -> page erase 0x81.
 *
 * HAZARD: 0x81 page erase is UNSUPPORTED on CH592A and hard-hangs it beyond SWD
 * recovery (CH592 datasheet §4.4; bench evidence in docs/AB-UPDATE.md). It IS
 * functional on CH592F (verified: firmware/tests/silicon/). CHIP=ch592 cannot
 * tell the A and F dies apart, so OB_FLASH_PAGE_ERASE is a deliberate opt-in by
 * which the builder asserts their die supports page erase — never a default. */
#if OB_FLASH_ERASE_BLOCK == 4096u
#define OB_FL_ERASE_OP 0x20u
#elif OB_FLASH_ERASE_BLOCK == 256u
#define OB_FL_ERASE_OP 0x81u
/* Page erase is CH592-only — the family the capability is proven on. This is
 * the hard C backstop for the Makefile CHIP=ch592 gate: it rejects ch57x
 * (ISP572 has no page-erase command) AND ch591 (build-validated only, never
 * bench-tested for 0x81), so even a mangled Make value (e.g. a trailing space
 * slipping past the CHIP guard) cannot compile a non-CH592 page-erase image. */
#if OB_CHIP_FAMILY != OB_FAMILY_CH592
#error "256 B page erase (0x81) is CH592-only"
#endif
#else
#error "OB_FLASH_ERASE_BLOCK must be 4096 or 256"
#endif

/* ---- transaction primitives ------------------------------------------- */
/* R8_FLASH_CTRL bit 7 is transaction-busy; 5 opens a byte transaction (the
 * serial engine clocks one byte per R8_FLASH_DATA access); 0x15 clocks one
 * byte out of the R32_FLASH_DATA word buffer during page program; 0 ends the
 * transaction and re-enables XIP; 4 is the controller-close the archives
 * issue before the resume. These polls are unbounded like the archives': they
 * wait on the byte engine (SCK-bounded), not on the die's program/erase. */

OB_FL_HIGHCODE("ob_fl_busy")
static void ob_fl_busy(void)
{
    while ((int8_t)OB_FL_CTRL_RD() < 0) {
    }
}

OB_FL_HIGHCODE("ob_fl_begin")
static void ob_fl_begin(uint8_t cmd)
{
    int i;

    OB_FL_CTRL_WR(0);
    OB_FL_CTRL_WR(0x05);
    for (i = 0; i < OB_FL_BEGIN_NOPS; i++) {
        OB_FL_NOP();
    }
    OB_FL_DATA8_WR(cmd);
}

OB_FL_HIGHCODE("ob_fl_end")
static void ob_fl_end(void)
{
    ob_fl_busy();
    OB_FL_CTRL_WR(0);
}

OB_FL_HIGHCODE("ob_fl_in")
static uint8_t ob_fl_in(void)
{
    ob_fl_busy();
    return OB_FL_DATA8_RD();
}

OB_FL_HIGHCODE("ob_fl_out")
static void ob_fl_out(uint8_t v)
{
    ob_fl_busy();
    OB_FL_DATA8_WR(v);
}

/* Gate open/close. R8_GLOB_ROM_CFG is SAM: RMW inside a safe-access window.
 * Close restores `reg & RB_ROM_CODE_OFS` — everything but the code-offset
 * bit cleared, exactly what both archives do (ch32fun keeps RB_ROM_CTRL_EN
 * instead; the archives are authoritative). */
OB_FL_HIGHCODE("ob_fl_open")
static void ob_fl_open(uint8_t gate)
{
    uint8_t glob = (uint8_t)(OB_FL_GLOB_RD() | gate);

    sys_safe_access_enable();
    OB_FL_GLOB_WR(glob);
    sys_safe_access_disable();
    OB_FL_CTRL_WR(0x04);
    /* 0xFF resume (release from power-down); ISP572 sends it twice with a
     * busy-wait between (FLASH_ROM_BEG_FF), ISP583 once. */
    ob_fl_begin(0xFF);
#if OB_FL_RESUME_TWICE
    ob_fl_busy();
    OB_FL_DATA8_WR(0xFF);
    ob_fl_busy();
#endif
    ob_fl_end();
}

OB_FL_HIGHCODE("ob_fl_close")
static void ob_fl_close(void)
{
    uint8_t glob = (uint8_t)(OB_FL_GLOB_RD() & RB_ROM_CODE_OFS);

    sys_safe_access_enable();
    OB_FL_GLOB_WR(glob);
    sys_safe_access_disable();
}

/* Send an opcode plus its 24-bit address. Read-class opcodes — fast read
 * 0x0B and unique-ID 0x4B, i.e. (cmd & 0xBF) == 0x0B — get two extra dummy
 * bytes (the address left-shift naturally feeds zeros) and no WREN; every
 * other opcode is a mutation and is preceded by WREN (0x06). */
OB_FL_HIGHCODE("ob_fl_cmd_addr")
static void ob_fl_cmd_addr(uint8_t cmd, uint32_t addr)
{
    int n = 5;

    if ((cmd & 0xBF) != 0x0B) {
        ob_fl_begin(0x06);
        ob_fl_end();
        n = 3;
    }
    ob_fl_begin(cmd);
    while (n--) {
        ob_fl_out((uint8_t)(addr >> 16));
        addr <<= 8;
    }
}

/* Poll the status register until write-in-progress clears. Returns
 * (status | 1) on success — never 0 — and 0 on timeout, the archives'
 * convention. Each iteration is a complete 0x05 transaction; the first
 * data byte is the engine's pipeline byte and is discarded. */
OB_FL_HIGHCODE("ob_fl_wait")
static uint32_t ob_fl_wait(void)
{
    uint32_t i;
    uint8_t status;

    ob_fl_end();
    for (i = OB_FL_WAIT_ITERS; i != 0; i--) {
        ob_fl_begin(0x05);
        (void)ob_fl_in();
        status = ob_fl_in();
        ob_fl_end();
        if ((status & 0x01) == 0) {
            return (uint32_t)status | 1u;
        }
    }
    return 0;
}

/* ---- operations -------------------------------------------------------- */
/* Every path that reaches ob_fl_open() leaves through ob_fl_close(): an
 * early return with the gate open would strand the part with XIP dead. */

OB_FL_HIGHCODE("ob_ch5xx_flash_erase")
uint32_t ob_ch5xx_flash_erase(uint32_t addr, uint32_t len)
{
    uint32_t rc = 0;

    if (len == 0 || ((addr | len) & (OB_FLASH_ERASE_BLOCK - 1u)) != 0 ||
        addr + len < addr) {
        return OB_FLERR_ERASE_PARAM;
    }
    /* One erase op per OB_FLASH_ERASE_BLOCK: sector erase 0x20 (4 KiB) by
     * default, or page erase 0x81 (256 B) when OB_FLASH_PAGE_ERASE lowers the
     * block — see the OB_FL_ERASE_OP hazard note above. The core erases exactly
     * one block per call; the loop is defensive. The archives also know 64 KiB
     * block erase (0xD8); the core never asks for it. */
    ob_fl_open(OB_FL_GATE_WRITE);
    while (len != 0) {
        ob_fl_cmd_addr(OB_FL_ERASE_OP, addr);
        if (ob_fl_wait() == 0) {
            rc = OB_FLERR_ERASE_TIMEOUT;
            break;
        }
        addr += OB_FLASH_ERASE_BLOCK;
        len -= OB_FLASH_ERASE_BLOCK;
    }
    ob_fl_close();
    return rc;
}

OB_FL_HIGHCODE("ob_ch5xx_flash_write")
uint32_t ob_ch5xx_flash_write(uint32_t addr, const void *buf, uint32_t len)
{
    const uint32_t *w = (const uint32_t *)buf;
    uint32_t words = len >> 2;
    uint32_t rc = 0;
    int i;

    if (len == 0 || ((addr | len) & 3u) != 0 || ((uintptr_t)buf & 3u) != 0 ||
        addr + len < addr) {
        return OB_FLERR_WRITE_PARAM;
    }
    ob_fl_open(OB_FL_GATE_WRITE);
    while (words != 0) {
        /* One 0x02 page program per 256-byte page: WREN + opcode + address,
         * then each word through the R32 buffer, four CTRL=0x15 byte clocks
         * per word. The archives break the stream when the address low byte
         * wraps to 0 — a program may start mid-page but never cross one. */
        ob_fl_cmd_addr(0x02, addr);
        do {
            OB_FL_DATA32_WR(*w++);
            for (i = 0; i < 4; i++) {
                ob_fl_busy();
                OB_FL_CTRL_WR(0x15);
            }
            words--;
            addr += 4;
        } while (words != 0 && (addr & 0xFFu) != 0);
        if (ob_fl_wait() == 0) {
            rc = OB_FLERR_WRITE_TIMEOUT;
            break;
        }
    }
    ob_fl_close();
    return rc;
}

OB_FL_HIGHCODE("ob_ch5xx_flash_verify")
uint32_t ob_ch5xx_flash_verify(uint32_t addr, const void *buf, uint32_t len)
{
    const uint32_t *w = (const uint32_t *)buf;
    uint32_t rc = 0;
    uint32_t i;

    if (len == 0 || ((addr | len) & 3u) != 0 || ((uintptr_t)buf & 3u) != 0 ||
        addr + len < addr) {
        return OB_FLERR_VERIFY_PARAM;
    }
    /* Controller fast read, deliberately not XIP: on CH57x the XIP path can
     * serve stale data after controller writes (F26); this read is coherent.
     * One R8 access clocks one byte; after every fourth byte the assembled
     * word is compared straight out of the R32 buffer, which does not clock. */
    ob_fl_open(OB_FL_GATE_READ);
    ob_fl_cmd_addr(0x0B, addr);
    for (i = 0; i < len; i++) {
        (void)ob_fl_in();
        if ((i & 3u) == 3u && OB_FL_DATA32_RD() != *w++) {
            rc = OB_FLERR_VERIFY_MISMATCH;
            break;
        }
    }
    ob_fl_end();
    ob_fl_close();
    return rc;
}

OB_FL_HIGHCODE("ob_ch5xx_flash_uid_read")
void ob_ch5xx_flash_uid_read(uint32_t buf[4])
{
    uint8_t *b = (uint8_t *)buf;
    int i;

    buf[0] = 0;
    buf[1] = 0;
    ob_fl_open(OB_FL_GATE_READ);
    ob_fl_cmd_addr(0x4B, 0);
    /* 16 bytes XOR-folded into 8: clocked byte k lands in b[(15 - k) & 7],
     * so b[j] = byte[7 - j] ^ byte[15 - j] — the archives' exact fold. */
    for (i = 15; i >= 0; i--) {
        b[i & 7] ^= ob_fl_in();
    }
    ob_fl_end();
    ob_fl_close();
}

OB_FL_HIGHCODE("ob_ch5xx_flash_rom_info_read")
void ob_ch5xx_flash_rom_info_read(uint32_t addr, uint32_t buf[4])
{
    uint32_t word;
    int i;

    ob_fl_open(OB_FL_GATE_READ);
    ob_fl_cmd_addr(0x0B, addr | OB_FL_INFO_OR);
    for (i = 0; i < 8; i++) {
        (void)ob_fl_in();
        if (i == 3) {
            buf[0] = OB_FL_DATA32_RD();
        }
    }
    word = OB_FL_DATA32_RD();
    if ((addr & (1u << 13)) != 0) {
        /* MAC window: 6-byte MAC — word at +0, halfword at +4, +6..7 kept.
         * Masked merge, not a uint16_t store: buf's effective type is
         * uint32_t and the halfword byte order is little-endian either way. */
        buf[1] = (buf[1] & 0xFFFF0000u) | (word & 0xFFFFu);
    } else {
        buf[1] = word;
    }
    ob_fl_end();
    ob_fl_close();
}
