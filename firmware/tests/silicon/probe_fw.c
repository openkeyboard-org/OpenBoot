/*
 * CH592 raw page-erase probe firmware — a standalone image whose only job is
 * to answer: does the 0x81 page-erase opcode complete on this die, or wedge?
 *
 * It is NOT the OpenBoot bootloader and shares no code with
 * ports/flash_ch5xx.c — every controller register write is spelled out here.
 * It reuses only the proven vectorless ch59x reset idiom (probe_start.S) so
 * the part is correctly brought up; the flash routines run from RAM
 * (.highcode) exactly as any correct implementation must, so opening the
 * write gate (which kills XIP) cannot hang the probe itself. That isolation
 * is the point: a wedge here is the silicon's, not the bootloader's.
 *
 * Sequence (main):
 *   STAGE=0xA0  sector-erase 0x20 the scratch sector          [CONTROL]
 *   STAGE=0xA1  program a known pattern into the scratch page (charge cells)
 *   STAGE=0xB0  page-erase   0x81 the scratch page            [TEST]
 *   STAGE=0xC0  done — both opcodes returned
 *   then idle, incrementing HB, for the debugger to read.
 *
 * Per-op markers (base B): B+0 phase 1..6, B+4 heartbeat, B+8 last status,
 * B+12 wait iterations consumed. Phase 4 = opcode+address issued, entering
 * the WIP poll; phase 5 = WIP cleared; phase 6 = gate closed. A control that
 * reaches 0xC0 with the sector marker at 6 and the page marker stuck at 4
 * (heartbeat frozen at the wait bound) is a page-erase wedge with the sector
 * erase — identical code, one opcode byte different — proven healthy first.
 *
 * Read the outcome over SWD after ~2 s: minichlink -r the marker block, and
 * -r the scratch flash to see what each opcode actually did to the cells.
 * Nothing here can brick; ROM-ISP recovers any state.
 */
#include <stdint.h>

#define REG8(a)   (*(volatile uint8_t  *)(a))
#define REG32(a)  (*(volatile uint32_t *)(a))

#define SAFE      0x40001040u
#define GLOB      0x40001044u
#define CHIPID    0x40001041u
#define FDATA32   0x40001800u
#define FDATA8    0x40001804u
#define FCTRL     0x40001806u

#define GATE_WR   0xE0u
#define GATE_KEEP 0x10u

#define M_STAGE   0x20005000u
#define M_CHIP    0x20005004u
#define M_HB      0x20005008u
#define M_SECTOR  0x20005010u   /* sector-erase marker block (4 words) */
#define M_PAGE    0x20005020u   /* page-erase marker block   (4 words) */

/* generous WIP bound: a healthy sector erase is ~17 ms, one poll iteration
 * is a full status transaction (>~64 cycles); at the 6.4 MHz reset clock
 * this is ~1 s, well past any real erase and enough to call a wedge. */
#define WAIT_ITERS  160000u

#define HC __attribute__((section(".highcode"), noinline))

HC static void busy(void) { while ((int8_t)REG8(FCTRL) < 0) { } }
HC static void begin(uint8_t c) { REG8(FCTRL) = 0; REG8(FCTRL) = 5;
                                  __asm__ volatile("nop"); REG8(FDATA8) = c; }
HC static void end(void)  { busy(); REG8(FCTRL) = 0; }
HC static void out(uint8_t v) { busy(); REG8(FDATA8) = v; }
HC static uint8_t in(void) { busy(); return REG8(FDATA8); }

HC static void gate_open(void)
{
    uint8_t g = (uint8_t)(REG8(GLOB) | GATE_WR);
    REG8(SAFE) = 0x57; REG8(SAFE) = 0xA8; REG8(GLOB) = g; REG8(SAFE) = 0x00;
    REG8(FCTRL) = 0x04;
    begin(0xFF); end();                     /* resume from power-down */
}

HC static void gate_open_read(void)
{
    uint8_t g = (uint8_t)(REG8(GLOB) | 0x20u); /* RB_ROM_CTRL_EN only */
    REG8(SAFE) = 0x57; REG8(SAFE) = 0xA8; REG8(GLOB) = g; REG8(SAFE) = 0x00;
    REG8(FCTRL) = 0x04;
    begin(0xFF); end();
}

HC static void gate_close(void)
{
    uint8_t g = (uint8_t)(REG8(GLOB) & GATE_KEEP);
    REG8(SAFE) = 0x57; REG8(SAFE) = 0xA8; REG8(GLOB) = g; REG8(SAFE) = 0x00;
}

HC static void cmd_addr(uint8_t cmd, uint32_t addr, int wren)
{
    if (wren) { begin(0x06); end(); }
    begin(cmd);
    out((uint8_t)(addr >> 16));
    out((uint8_t)(addr >> 8));
    out((uint8_t)addr);
}

/* One 32-bit word read via 0x0B fast read (controller, not XIP). INDICATIVE
 * ONLY: this minimal framing does not reproduce the controller's exact
 * fast-read clocking (a bench check read flash word 0 as 0x52460579 where
 * minichlink reads the true 0x0040006f), so its ABSOLUTE values are not
 * trustworthy. It still cleanly distinguishes programmed from erased cells
 * (clearly different values), which is all the in-firmware witnesses use it
 * for; authoritative flash reads in this investigation are done with
 * minichlink, not this. */
HC static uint32_t read_word(uint32_t addr)
{
    uint32_t v;
    gate_open_read();
    begin(0x0B);
    out((uint8_t)(addr >> 16));
    out((uint8_t)(addr >> 8));
    out((uint8_t)addr);
    out(0); out(0);                         /* two dummy bytes */
    (void)in();                             /* engine pipeline byte, discard */
    v  = (uint32_t)in();
    v |= (uint32_t)in() << 8;
    v |= (uint32_t)in() << 16;
    v |= (uint32_t)in() << 24;
    end();
    gate_close();
    return v;
}

/* One erase op with full marker instrumentation into the block at `mark`. */
HC static void erase_op(uint8_t cmd, uint32_t addr, uint32_t mark)
{
    uint32_t i, hb = 0;
    uint8_t st;

    gate_open();
    REG32(mark) = 1;
    cmd_addr(cmd, addr, 1);
    REG32(mark) = 4;                        /* opcode+addr issued */
    end();
    for (i = WAIT_ITERS; i != 0; i--) {
        REG32(mark + 4) = ++hb;
        begin(0x05);
        busy(); (void)REG8(FDATA8);
        busy(); st = REG8(FDATA8);
        REG32(mark + 8) = st;
        end();
        if ((st & 0x01) == 0) { break; }
    }
    REG32(mark + 12) = WAIT_ITERS - i;      /* iterations consumed */
    REG32(mark) = (i == 0) ? 40 : 5;        /* 40 = WIP-timeout, 5 = cleared */
    gate_close();
    if (i != 0) { REG32(mark) = 6; }        /* 6 = closed / done */
}

/* Program one 256 B page with a walking pattern to charge the cells. */
HC static void program_page(uint32_t addr)
{
    uint32_t w;
    int i;

    gate_open();
    cmd_addr(0x02, addr, 1);
    for (w = 0; w < 64; w++) {              /* 64 words = 256 B */
        REG32(FDATA32) = 0xA5A5A500u | w;
        for (i = 0; i < 4; i++) { busy(); REG8(FCTRL) = 0x15; }
    }
    /* wait WIP */
    for (i = 0; i < (int)WAIT_ITERS; i++) {
        begin(0x05); busy(); (void)REG8(FDATA8); busy();
        uint8_t st = REG8(FDATA8); end();
        if ((st & 0x01) == 0) { break; }
    }
    gate_close();
}

/* Witness block for the 4-page discrimination run (base 0x20005040):
 *   +0x00 .. +0x0c : page 0..3 readback AFTER programming
 *   +0x10 .. +0x1c : page 0..3 readback AFTER the 0x81 page-erase of page 1
 * Distinguishes: hang (STAGE stuck, witnesses absent) vs true page erase
 * (only page 1 erased) vs sector-erase alias (all four erased). */
#define W_PROG  0x20005040u
#define W_AFTER 0x20005050u

int main(void)
{
    const uint32_t sector = 0x0006F000u;    /* top sector, clear of the app */
    int p;

    REG32(M_CHIP)  = REG8(CHIPID);

    /* Part 1: the original control+test on page 0, fully instrumented. */
    REG32(M_STAGE) = 0xA0;
    erase_op(0x20, sector, M_SECTOR);       /* CONTROL: sector erase */
    REG32(M_STAGE) = 0xA1;
    program_page(sector);                   /* charge the cells */
    REG32(M_STAGE) = 0xB0;
    erase_op(0x81, sector, M_PAGE);         /* TEST: page erase */

    /* Part 2: does 0x81 erase ONE page or the whole sector? Program all
     * four pages of the sector with distinct patterns, confirm they took,
     * page-erase ONLY page 1, then read all four back. */
    REG32(M_STAGE) = 0xB1;
    erase_op(0x20, sector, M_SECTOR);       /* clean slate */
    for (p = 0; p < 4; p++) { program_page(sector + (uint32_t)p * 256u); }
    for (p = 0; p < 4; p++) {
        REG32(W_PROG + (uint32_t)p * 4u) = read_word(sector + (uint32_t)p * 256u);
    }
    REG32(M_STAGE) = 0xB2;
    erase_op(0x81, sector + 256u, M_PAGE);  /* erase page 1 only */
    for (p = 0; p < 4; p++) {
        REG32(W_AFTER + (uint32_t)p * 4u) = read_word(sector + (uint32_t)p * 256u);
    }

    /* Read-path validation + like-for-like erased-state comparison, all via
     * the firmware's own 0x0B fast read:
     *   W_CODE0 : this image's flash word 0 — cross-checkable against the .bin
     *             (0x0040006f) and minichlink, so the read path is trusted.
     *   W_SE20  : a spare sector freshly 0x20-erased, read the same way as the
     *             0x81-erased page above (W_AFTER page 1). */
    REG32(0x20005060u) = read_word(0x00000000u);          /* W_CODE0 */
    erase_op(0x20, 0x0006E000u, M_SECTOR);                /* spare sector */
    REG32(0x20005064u) = read_word(0x0006E000u);          /* W_SE20 */

    REG32(M_STAGE) = 0xC0;                   /* everything returned */
    for (uint32_t hb = 0;;) { REG32(M_HB) = ++hb; }
}
