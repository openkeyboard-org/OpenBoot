/* Simulated CH5xx flash + mock port hooks for the host-native core tests.
 *
 * Models the properties the core's safety story depends on:
 *  - erase granularity + "write only into blocks erased this power cycle"
 *    (breaches recorded in a violation counter, never fatal);
 *  - F26 stale XIP reads: blocks modified since the last reset serve their
 *    pre-modification content through OB_XIP_READ* while true content is
 *    only visible to ob_flash_verify/host_flash_read (controller path);
 *  - power-cut injection: after N mutating ops every further op fails; a
 *    cut ob_record_write leaves a torn (half-written) record.
 */
#include <setjmp.h>
#include <stdlib.h>
#include <string.h>

#include "ob_host.h"
#include "openboot_port.h"
#include "boot_core.h"
#include "boot_decision.h"

#define SIM_BLOCKS (OB_FLASH_APP_END / OB_FLASH_ERASE_BLOCK)

static uint8_t  sim_flash[OB_FLASH_APP_END]; /* true content, [0, app_end) */
static uint8_t  sim_stale[OB_FLASH_APP_END]; /* pre-modification snapshots */
static uint8_t  blk_erased[SIM_BLOCKS];      /* erased this power cycle */
static uint8_t  blk_dirty[SIM_BLOCKS];       /* modified this power cycle */
_Static_assert(OB_BOOT_RECORD_SIZE == sizeof(ob_boot_record_t),
               "simulated record must match ob_boot_record_t");
static uint32_t violations;
static uint32_t op_total;
static int32_t  fail_after = -1;
static int32_t  bootpin;
static uint32_t sim_silicon_app_end;   /* 0 = unknown chip id */
#if defined(OB_HOST_CH57X)
#define OB_HOST_F26_DEFAULT 1
#else
#define OB_HOST_F26_DEFAULT 0
#endif
static int32_t  f26_mode = OB_HOST_F26_DEFAULT;
static uint32_t jumped_to;   /* slot base the boot decision chose */

uint32_t ob_host_bootreq;

static jmp_buf jump_env;
static int     jump_armed;

static void fill_erased(uint8_t *dst, uint32_t addr, uint32_t len)
{
    uint32_t i;

    for (i = 0; i < len; i++)
        dst[i] = (uint8_t)(OB_ERASED_WORD >> (8u * ((addr + i) & 3u)));
}

static void touch_block(uint32_t blk)
{
    if (!blk_dirty[blk]) {
        memcpy(&sim_stale[blk * OB_FLASH_ERASE_BLOCK],
               &sim_flash[blk * OB_FLASH_ERASE_BLOCK], OB_FLASH_ERASE_BLOCK);
        blk_dirty[blk] = 1;
    }
}

/* F26 applies to the record page too: XIP-style reads after a controller
 * write serve the pre-modification record until the next reset. */
/* One mutating flash op. Nonzero = power is out, op must fail. */
static int op_cut(void)
{
    op_total++;
    if (fail_after < 0)
        return 0;
    if (fail_after == 0)
        return 1;
    fail_after--;
    return 0;
}

/* ---- XIP interception ------------------------------------------------- */

uint8_t ob_host_xip_read8(uint32_t addr)
{
    if (addr >= OB_FLASH_APP_END) {
        violations++;                    /* core must never XIP-read outside */
        return 0xFF;
    }
    if (f26_mode && blk_dirty[addr / OB_FLASH_ERASE_BLOCK])
        return sim_stale[addr];
    return sim_flash[addr];
}

uint32_t ob_host_xip_read32(uint32_t addr)
{
    return (uint32_t)ob_host_xip_read8(addr) |
           ((uint32_t)ob_host_xip_read8(addr + 1) << 8) |
           ((uint32_t)ob_host_xip_read8(addr + 2) << 16) |
           ((uint32_t)ob_host_xip_read8(addr + 3) << 24);
}

/* ---- port hooks ------------------------------------------------------- */

void ob_port_init(void) {}

uint32_t ob_flash_erase(uint32_t addr, uint32_t len)
{
    uint32_t a;

    if (op_cut())
        return 0xE0;
    if ((addr % OB_FLASH_ERASE_BLOCK) || (len % OB_FLASH_ERASE_BLOCK) || len == 0)
        violations++;
    if (addr < OB_FLASH_APP_START || addr >= OB_FLASH_APP_END ||
        len > OB_FLASH_APP_END - addr) {
        violations++;                    /* would erase outside the app region */
        return 0;
    }
    for (a = addr; a + OB_FLASH_ERASE_BLOCK <= addr + len; a += OB_FLASH_ERASE_BLOCK) {
        uint32_t blk = a / OB_FLASH_ERASE_BLOCK;

        touch_block(blk);
        fill_erased(&sim_flash[a], a, OB_FLASH_ERASE_BLOCK);
        blk_erased[blk] = 1;
    }
    return 0;
}

uint32_t ob_flash_write(uint32_t addr, const void *buf, uint32_t len)
{
    uint32_t b;

    if (op_cut())
        return 0xE2;
    if ((addr % 4u) || (len % 4u) || len == 0 || ((uintptr_t)buf & 3u))
        violations++;
    if (addr < OB_FLASH_APP_START || addr >= OB_FLASH_APP_END ||
        len > OB_FLASH_APP_END - addr) {
        violations++;                    /* would write outside the app region */
        return 0;
    }
    for (b = addr / OB_FLASH_ERASE_BLOCK;
         b <= (addr + len - 1) / OB_FLASH_ERASE_BLOCK; b++) {
        if (!blk_erased[b])
            violations++;                /* write into a non-erased block */
        touch_block(b);
    }
    memcpy(&sim_flash[addr], buf, len);
    return 0;
}

uint32_t ob_flash_verify(uint32_t addr, const void *buf, uint32_t len)
{
    if (addr < OB_FLASH_APP_START || addr >= OB_FLASH_APP_END ||
        len > OB_FLASH_APP_END - addr) {
        violations++;
        return 0xE4;
    }
    /* Controller read path: always coherent, F26 does not apply. */
    return memcmp(&sim_flash[addr], buf, len) ? 0xE5 : 0;
}

int ob_bootpin_asserted(void)
{
    return bootpin;
}

/* Mirrors the real ports: the app end comes from the silicon, clamped by the
 * build. host_set_chip_id() lets a test pretend to be the wrong variant. */
uint32_t ob_app_end(void)
{
    uint32_t silicon = sim_silicon_app_end;

    if (silicon == 0)
        return OB_FLASH_APP_END;        /* unknown id: trust the build */
    return silicon < OB_FLASH_APP_END ? silicon : OB_FLASH_APP_END;
}

uint8_t ob_chip_rev(void)
{
    return 9;                            /* CH592A value used by the goldens */
}

void ob_read_uid(uint8_t uid[8])
{
    static const uint8_t u[8] = {0xEF, 0xCD, 0xAB, 0x89, 0x67, 0x45, 0x23, 0x01};

    memcpy(uid, u, 8);                   /* 0x0123456789ABCDEF LE */
}

void ob_delay_us(uint32_t us)
{
    (void)us;
}

/* Test-controlled clock. main.c is the only caller in the firmware and is
 * not linked here, so this exists to satisfy the port contract and to let a
 * test drive ob_idle_elapsed() against a chosen "now". */
static uint32_t host_now_ms;   /* zeroed by both reset paths */

uint32_t ob_uptime_ms(void)
{
    return host_now_ms;
}

void host_set_uptime_ms(uint32_t v)
{
    host_now_ms = v;
}

void ob_jump_app(uint32_t base)
{
    jumped_to = base;
    if (jump_armed)
        longjmp(jump_env, 1);
    abort();                             /* jump outside host_boot_decide_result */
}

void ob_reset(void)
{
    abort();                             /* harness acts on the action code instead */
}

/* ---- harness API ------------------------------------------------------ */

void host_reset(void)
{
    host_now_ms = 0;
    fill_erased(sim_flash, 0, sizeof sim_flash);
    memset(blk_erased, 0, sizeof blk_erased);
    memset(blk_dirty, 0, sizeof blk_dirty);
    violations = 0;
    op_total = 0;
    fail_after = -1;
    bootpin = 0;
    f26_mode = OB_HOST_F26_DEFAULT;
    sim_silicon_app_end = OB_FLASH_APP_END;   /* matching part by default */
    ob_host_bootreq = 0;
    ob_core_init();
}

void host_power_cycle(void)
{
    host_now_ms = 0;
    memset(blk_erased, 0, sizeof blk_erased);   /* erases don't survive reset */
    memset(blk_dirty, 0, sizeof blk_dirty);     /* XIP coherent again */
    fail_after = -1;
    ob_host_bootreq = 0;                        /* RAM lost */
    ob_core_init();
}

void host_frame(const uint8_t *in, uint32_t inlen,
                uint8_t *out, uint32_t *outlen, int32_t *action)
{
    static uint8_t inbuf[64] __attribute__((aligned(4)));
    uint32_t rlen = 0;

    if (inlen > sizeof inbuf)
        inlen = sizeof inbuf;
    memcpy(inbuf, in, inlen);            /* core requires a 4-aligned frame */
    *action = (int32_t)ob_core_handle_frame(inbuf, inlen, out, &rlen);
    *outlen = rlen;
}

void host_flash_read(uint32_t addr, uint8_t *buf, uint32_t len)
{
    if (addr >= sizeof sim_flash)
        return;
    if (len > sizeof sim_flash - addr)
        len = (uint32_t)(sizeof sim_flash) - addr;
    memcpy(buf, &sim_flash[addr], len);
}

void host_write_flash(uint32_t addr, const uint8_t *buf, uint32_t len)
{
    /* Bypasses the controller entirely: models bytes that arrived by SWD or
     * a factory image, so a test can stage a slot without driving OBP. */
    memcpy(&sim_flash[addr], buf, len);
}

uint32_t host_jumped_to(void)
{
    return jumped_to;
}

void host_record_raw(uint32_t slot, uint8_t out[OB_BOOT_RECORD_SIZE])
{
    memcpy(out, &sim_flash[ob_slot_record_addr(slot)], OB_BOOT_RECORD_SIZE);
}

void host_set_record_raw(uint32_t slot, const uint8_t in[OB_BOOT_RECORD_SIZE])
{
    memcpy(&sim_flash[ob_slot_record_addr(slot)], in, OB_BOOT_RECORD_SIZE);
}

void host_set_fail_after(int32_t n) { fail_after = n; }
uint32_t host_violations(void)      { return violations; }
uint32_t host_op_total(void)        { return op_total; }
void host_set_bootreq(uint32_t v)   { ob_host_bootreq = v; }
uint32_t host_get_bootreq(void)     { return ob_host_bootreq; }
void host_set_bootpin(int32_t v)    { bootpin = v; }
void host_set_silicon_app_end(uint32_t v) { sim_silicon_app_end = v; }
void host_set_f26(int32_t v)        { f26_mode = v; }

int32_t host_boot_decide_result(void)
{
    volatile int32_t r;

    jump_armed = 1;
    if (setjmp(jump_env)) {
        r = 0;                           /* ob_jump_app reached */
    } else {
        ob_boot_decide();
        r = 1;                           /* returned: stay in bootloader */
    }
    jump_armed = 0;
    return r;
}
