/* Recording register mock behind ob_flash_host_port.h's OB_FL_* macros.
 *
 * Every access the driver makes becomes one event (kind, value); DATA8 reads
 * pull from a Python-scripted FIFO with a settable default. GLOB is modeled
 * as a real register (writes stick, reads return the last write) so tests
 * can prove close restores exactly what open read.
 *
 * Two pieces of physics are modeled rather than scripted, so a driver that
 * skips them fails EVERY test instead of only a bespoke one:
 *
 *  - The byte engine's busy bit. Each R8_FLASH_DATA access and each
 *    CTRL=0x15 pulse starts a byte transfer; the next synchronized access
 *    (R8 data, a pulse, or the CTRL=0 that closes the transaction) is only
 *    legal after a busy poll (CTRL read) or a fresh CTRL=5. Violations are
 *    counted, and the test fixture asserts the count is zero — deleting any
 *    ob_fl_busy() in the driver trips it.
 *
 *  - R32_FLASH_DATA assembles from the last four clocked bytes (LE), like
 *    the controller's word buffer. Verify/rom-info tests script only the
 *    byte stream; a compare at the wrong phase reads a partially-assembled
 *    word and mismatches.
 *
 * Exercised from tests/test_flash_driver.py over ctypes. */
#include <stdint.h>

uint32_t ob_host_bootreq;   /* referenced by the port header's BOOTREQ addr */

enum {
    EV_CTRL_WR = 1, EV_DATA8_WR, EV_DATA32_WR, EV_GLOB_WR,
    EV_CTRL_RD, EV_DATA8_RD, EV_DATA32_RD, EV_GLOB_RD,
    EV_NOP, EV_SAFE_ON, EV_SAFE_OFF,
};

#define EVCAP  65536
#define QCAP   4096

uint8_t  ob_flmock_ev_kind[EVCAP];
uint32_t ob_flmock_ev_val[EVCAP];
uint32_t ob_flmock_ev_count;
uint32_t ob_flmock_ev_lost;         /* nonzero => the log overflowed */
uint32_t ob_flmock_poll_violations; /* synchronized access while busy */

static uint8_t  rd8_q[QCAP];
static uint32_t rd8_head, rd8_tail;
static uint8_t  rd8_default;
static uint8_t  glob_value;
static uint32_t r32_shift;          /* word buffer fed by byte clocks */
static int      engine_busy;

static void ev(uint8_t kind, uint32_t val)
{
    if (ob_flmock_ev_count < EVCAP) {
        ob_flmock_ev_kind[ob_flmock_ev_count] = kind;
        ob_flmock_ev_val[ob_flmock_ev_count] = val;
        ob_flmock_ev_count++;
    } else {
        ob_flmock_ev_lost++;
    }
}

static void need_ready(void)
{
    if (engine_busy) {
        ob_flmock_poll_violations++;
    }
}

void ob_flmock_reset(uint8_t glob_initial, uint8_t rd8_def)
{
    ob_flmock_ev_count = 0;
    ob_flmock_ev_lost = 0;
    ob_flmock_poll_violations = 0;
    rd8_head = rd8_tail = 0;
    glob_value = glob_initial;
    rd8_default = rd8_def;
    r32_shift = 0;
    engine_busy = 0;
}

void ob_flmock_push_rd8(uint8_t v)
{
    if (rd8_tail < QCAP) {
        rd8_q[rd8_tail++] = v;
    }
}

uint8_t ob_flmock_ctrl_rd(void)
{
    ev(EV_CTRL_RD, 0);
    engine_busy = 0;                /* the poll observed ready */
    return 0;
}

void ob_flmock_ctrl_wr(uint8_t v)
{
    ev(EV_CTRL_WR, v);
    if (v == 0x05) {
        engine_busy = 0;            /* fresh transaction */
    } else if (v == 0x15) {
        need_ready();               /* pulse clocks one byte out of R32 */
        engine_busy = 1;
    } else if (v == 0x00) {
        need_ready();               /* closing a live engine loses a byte */
    }
}

uint8_t ob_flmock_data8_rd(void)
{
    uint8_t v = (rd8_head < rd8_tail) ? rd8_q[rd8_head++] : rd8_default;

    need_ready();                   /* a read clocks the next byte */
    engine_busy = 1;
    r32_shift = (r32_shift >> 8) | ((uint32_t)v << 24);
    ev(EV_DATA8_RD, v);
    return v;
}

void ob_flmock_data8_wr(uint8_t v)
{
    need_ready();                   /* a write clocks this byte */
    engine_busy = 1;
    ev(EV_DATA8_WR, v);
}

uint32_t ob_flmock_data32_rd(void)
{
    ev(EV_DATA32_RD, r32_shift);    /* passive: no clocking, no busy rule */
    return r32_shift;
}

void ob_flmock_data32_wr(uint32_t v)
{
    ev(EV_DATA32_WR, v);            /* loads the word buffer; pulses clock it */
}

uint8_t ob_flmock_glob_rd(void)
{
    ev(EV_GLOB_RD, glob_value);
    return glob_value;
}

void ob_flmock_glob_wr(uint8_t v)
{
    glob_value = v;
    ev(EV_GLOB_WR, v);
}

void ob_flmock_nop(void)            { ev(EV_NOP, 0); }
void ob_flmock_safe(int on)         { ev(on ? EV_SAFE_ON : EV_SAFE_OFF, 0); }
