/* Bootloader main loop: fully polled, no interrupts, no timers. Every
 * iteration ends with exactly one OB_POLL_INTERVAL_US sleep, and the
 * count of those iterations is the only time source there is.
 *
 * What that buys: no traffic can RESET the idle timer — every iteration
 * advances it, whether the frame was valid, rejected, malformed or
 * absent — so only a real session keeps a device out of its app.
 *
 * What it does not buy: iterations are not equal in length, so this is
 * not a wall clock. An iteration that handles a frame also runs the
 * handler and tr_send() — on USB a send can block for milliseconds
 * against a host that is not draining — yet still credits one nominal
 * 20 us slot. Sustained framed traffic therefore stretches the real
 * timeout, potentially by orders of magnitude. Bounding it in seconds
 * needs a monotonic counter the port layer does not currently expose;
 * OB_IDLE_TIMEOUT_MS is a floor on a quiet link, not a deadline. */
#include "openboot_port.h"
#include "openboot_transport.h"
#include "boot_core.h"
#include "boot_decision.h"

#ifndef OB_IDLE_TIMEOUT_MS               /* board-overridable via build */
#define OB_IDLE_TIMEOUT_MS 10000u
#endif
#define OB_IDLE_POLLS OB_MS_TO_POLLS(OB_IDLE_TIMEOUT_MS)
#if OB_IDLE_TIMEOUT_MS != 0
_Static_assert(OB_IDLE_POLLS > 0, "enabled idle timeout must take at least one poll");
_Static_assert(OB_IDLE_POLLS <= UINT32_MAX, "idle timeout poll count exceeds uint32_t");
#endif

int main(void)
{
    uint8_t resp[OB_MAX_FRAME];
#if OB_IDLE_TIMEOUT_MS != 0
    uint32_t idle = 0;
#endif

    ob_port_init();
    ob_boot_decide();                    /* may not return */
    tr_init();
    ob_core_init();

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

        /* Idle auto-boot counts every iteration, so no traffic of any
         * kind RESETS the timer (see the file header for why that is
         * not the same as a wall clock); only an active session
         * (successful HELLO) suppresses it, until the next reset.
         * OB_IDLE_TIMEOUT_MS == 0 compile-disables the whole block (a
         * preprocessor guard, not a folded runtime condition: -Werror's
         * -Wtype-limits rejects `++idle >= 0` even when unreachable).
         * This is the one direct ob_jump_app() outside the boot
         * decision: it can never run on dirty flash — mutation requires
         * a session and an active session suppresses the timeout — so
         * XIP is coherent here by construction. */
#if OB_IDLE_TIMEOUT_MS != 0
        if (!ob_core_session_active() && ++idle >= OB_IDLE_POLLS) {
            if (ob_boot_app_valid()) {
                tr_deinit();
                ob_jump_app();
            }
            idle = 0;                    /* invalid app: recheck next period */
        }
#endif

        ob_delay_us(OB_POLL_INTERVAL_US);
    }
}
