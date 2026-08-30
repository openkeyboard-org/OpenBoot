/* Bootloader main loop: fully polled, no interrupts, no ISRs. Every
 * iteration ends with one OB_POLL_INTERVAL_US sleep; the idle deadline is
 * measured against ob_uptime_ms(), a free-running SysTick counter the port
 * starts once the clock is settled.
 *
 * This used to count poll iterations and call the result milliseconds. It
 * was not a wall clock and the drift was severe — bench-measured at ~8.6x
 * on ch592-usb and ~27x on ch570-usb — because iterations are not equal in
 * length: one that handles a frame also runs the handler and tr_send(),
 * which on USB can block for milliseconds against a host that is not
 * draining, yet still credited one nominal 20 us slot.
 *
 * The property that mattered is preserved: no traffic RESETS the deadline.
 * It is anchored once and only an active session (successful HELLO)
 * suppresses it, so nothing short of a session keeps a device out of its
 * app. Reading a clock instead of counting iterations changes only whether
 * the number means what it says. */
#include "openboot_port.h"
#include "openboot_transport.h"
#include "boot_core.h"
#include "boot_decision.h"

#ifndef OB_IDLE_TIMEOUT_MS               /* board-overridable via build */
#define OB_IDLE_TIMEOUT_MS 10000u
#endif

/* Ceiling on consecutive hot polls while tr_rx_busy(): a legitimate frame
 * needs well under a hundred (62 wire bytes is ~5.4 ms, one FIFO drain per
 * pass), so this only ever binds against a host that streams bytes forever —
 * which must not be able to suppress the idle deadline below. */
#define OB_BUSY_POLLS_MAX 4096u

int main(void)
{
    uint8_t resp[OB_MAX_FRAME];
    uint32_t idle_start, now_ms;
    uint32_t busy_polls = 0;

    ob_port_init();
    ob_boot_decide();                    /* may not return */
    tr_init();
    ob_core_init();

    /* Anchor after bring-up, so a slow boot-time image CRC does not eat
     * into the connection window the board asked for. */
    idle_start = ob_uptime_ms();

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
        /* Mid-frame, stay hot: the bookkeeping below plus the sleep is one
         * loop pass, and on the slowest supported XIP (CH570 at the UART
         * image's 6.4 MHz) a pass measured ~410 us — more than half the
         * 8-byte RX FIFO's 700 us of headroom at 115200, which lost bytes
         * out of any continuous run longer than ~20 wire bytes. Bounded so
         * a host that streams bytes forever still meets the idle deadline
         * and the ~43 s clock fold below. */
        if (tr_rx_busy() && ++busy_polls < OB_BUSY_POLLS_MAX)
            continue;
        busy_polls = 0;

        /* Read the clock unconditionally, before any short-circuit can skip
         * it: ob_uptime_ms() folds the hardware counter into a millisecond
         * total and that counter wraps every ~43 s, so a caller that stops
         * polling during a long session would silently lose time. Nothing
         * depends on that today — a session suppresses the timeout for the
         * rest of the power cycle — but a future session-quiet deadline
         * would, and the bug would not be visible until then. */
        now_ms = ob_uptime_ms();

        if (!ob_core_session_active() &&
            ob_idle_elapsed(idle_start, now_ms, OB_IDLE_TIMEOUT_MS)) {
            uint32_t slot = ob_boot_select(0);

            if (slot != OB_SLOT_NONE) {
                tr_deinit();
                ob_jump_app(ob_slot_base(slot));
            }
            idle_start = now_ms;         /* nothing bootable: recheck later */
        }

        ob_delay_us(OB_POLL_INTERVAL_US);
    }
}
