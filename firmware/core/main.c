/* Bootloader main loop: fully polled, no interrupts, no ISRs. Every
 * iteration ends with one OB_POLL_INTERVAL_US sleep; the idle deadline is
 * measured against ob_now_ticks(), the free-running SysTick counter the port
 * starts once the clock is settled.
 *
 * The deadline is a raw tick compare: anchor a start tick once, and fire when
 * (now - start) reaches OB_IDLE_TIMEOUT_TICKS — the millisecond knob converted
 * to ticks at OB_CPU_HZ. The SysTick counts at HCLK, so this is exact, and the
 * deadline is a few seconds, far inside the ~43 s counter wrap, so one
 * unsigned subtraction is all it takes. (An earlier version counted poll
 * iterations and called them milliseconds; iterations are not equal in length
 * — a frame handler and tr_send() can block for milliseconds yet credit one
 * nominal 20 us slot — so the drift was severe, ~8.6x on ch592-usb and ~27x on
 * ch570-usb. The time base is the hardware counter for that reason.)
 *
 * The property that mattered is preserved: no traffic RESETS the deadline. It
 * is anchored once and only an active session (successful HELLO) suppresses
 * it, so nothing short of a session keeps a device out of its app. */
#include "openboot_port.h"
#include "openboot_transport.h"
#include "boot_core.h"
#include "boot_decision.h"

#ifndef OB_IDLE_TIMEOUT_MS               /* board-overridable via build */
#define OB_IDLE_TIMEOUT_MS 10000u
#endif

/* The deadline as a SysTick tick count. OB_CPU_HZ is in Hz and divides cleanly
 * by 1000 on every supported clock, so ticks-per-ms is exact. ob_idle_elapsed
 * compares (now - start) in 32-bit unsigned arithmetic, which stays correct
 * only while the true elapsed is below one 2^32-tick wrap AND some read lands
 * in [timeout, 2^32) — otherwise a large gap between reads could step the delta
 * past the whole window and miss the deadline for a full wrap. The loop reads
 * every pass (worst case tens of ms apart, e.g. a USB send wait), so cap the
 * deadline at half the counter: >= 2^31 ticks of headroom then always remain,
 * orders of magnitude more than any inter-read gap. The practical effect is a
 * clock-dependent maximum on OB_IDLE_TIMEOUT_MS — ~21 s at 100 MHz, ~36 s at
 * 60 MHz, ~335 s at 6.4 MHz; the 10 s default and any realistic idle window sit
 * far inside it (see firmware/README.md). */
#define OB_IDLE_TIMEOUT_TICKS \
    ((uint32_t)((uint64_t)(OB_IDLE_TIMEOUT_MS) * (OB_CPU_HZ / 1000u)))
_Static_assert(OB_CPU_HZ % 1000u == 0u, "OB_CPU_HZ must be a whole number of kHz");
_Static_assert(OB_IDLE_TIMEOUT_MS == 0u ||
               (uint64_t)(OB_IDLE_TIMEOUT_MS) * (OB_CPU_HZ / 1000u) <= 0x80000000u,
               "OB_IDLE_TIMEOUT_MS too large for this OB_CPU_HZ: the tick deadline "
               "must stay <= 2^31 to keep the wrap-safe compare's margin "
               "(max ~21 s at 100 MHz, ~36 s at 60 MHz, ~335 s at 6.4 MHz)");

/* Ceiling on consecutive hot polls while tr_rx_busy(): a legitimate frame
 * needs well under a hundred (62 wire bytes is ~5.4 ms, one FIFO drain per
 * pass), so this only ever binds against a host that streams bytes forever —
 * which must not be able to suppress the idle deadline below. */
#define OB_BUSY_POLLS_MAX 4096u

int main(void)
{
    uint8_t resp[OB_MAX_FRAME];
    uint32_t idle_start;
    uint32_t busy_polls = 0;

    ob_port_init();
    ob_boot_decide();                    /* may not return */
    tr_init();
    ob_core_init();

    /* Anchor after bring-up, so a slow boot-time image CRC does not eat
     * into the connection window the board asked for. */
    idle_start = ob_now_ticks();

    for (;;) {
        uint32_t avail, rlen;
        const uint8_t *frm = tr_poll(&avail);

        if (frm) {
            ob_action_t act = ob_core_handle_frame(frm, avail, resp, &rlen);

            if (rlen)
                tr_send(resp, rlen);     /* respond before acting */
            if (act != OB_ACT_NONE) {
                tr_deinit();
                ob_reset();              /* the boot decision launches */
            }
        }

        /* Idle auto-boot. The deadline is anchored once and no traffic of
         * any kind RESETS it — valid, rejected, malformed and CRC-corrupt
         * frames alike leave it alone. Only an active session (successful
         * HELLO) suppresses it, until the next reset.
         * ob_idle_elapsed() treats OB_IDLE_TIMEOUT_MS == 0 as disabled, so
         * no preprocessor guard is needed here.
         * This is the one direct ob_jump_app() outside the boot decision:
         * it can never run on dirty flash — mutation requires a session and
         * an active session suppresses the timeout — so XIP is coherent
         * here by construction. */
        /* Mid-frame, skip the idle check: the tick read and compare below
         * cost more than the sleep itself, and on the slowest supported XIP
         * (CH570 at the UART image's 6.4 MHz) the full pass measured ~410 us
         * — more than half the 8-byte RX FIFO's 700 us of headroom at 115200,
         * which lost bytes out of any continuous run longer than ~20 wire
         * bytes. The OB_POLL_INTERVAL_US sleep is deliberately KEPT on this
         * path: transports convert milliseconds to poll counts with it
         * (UART_IDLE_POLLS), so a sleepless hot loop would burn the 50 ms
         * inter-byte allowance in a few ms of empty polls on PLL-clocked
         * builds. Bounded (OB_BUSY_POLLS_MAX) so a host that streams bytes
         * forever still meets the idle deadline. */
        if (tr_rx_busy() && ++busy_polls < OB_BUSY_POLLS_MAX) {
            ob_delay_us(OB_POLL_INTERVAL_US);
            continue;
        }
        busy_polls = 0;

        if (!ob_core_session_active()) {
            uint32_t now = ob_now_ticks();   /* one read: compare and re-anchor */

            if (ob_idle_elapsed(idle_start, now, OB_IDLE_TIMEOUT_TICKS)) {
                uint32_t slot = ob_boot_select(0);

                if (slot != OB_SLOT_NONE) {
                    tr_deinit();
                    ob_jump_app(ob_slot_base(slot));
                }
                idle_start = now;            /* nothing bootable: recheck later */
            }
        }

        ob_delay_us(OB_POLL_INTERVAL_US);
    }
}
