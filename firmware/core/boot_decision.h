/* Reset-time boot decision: runs before transport init, decides whether to
 * stay in the bootloader or jump to the app. */
#ifndef OB_BOOT_DECISION_H
#define OB_BOOT_DECISION_H

#include "../../protocol/openboot_protocol.h"

/* Returns only when staying in the bootloader; otherwise calls
 * ob_jump_app(). The RAM boot-request magic is read and cleared first, so it
 * is always consumed. Decision priority is then: strap pin (debounced), boot
 * request, boot record, optional full image CRC (OB_BOOT_IMAGE_CRC), erased
 * first app word. */
void ob_boot_decide(void);

/* Record + image checks only (no strap/bootreq, no side effects). Used by
 * the pre-HELLO idle timeout and the explicit-BOOT validation. */
int ob_boot_app_valid(void);

/* The canonical record-validity check (PROTOCOL.md section 9.1): read
 * success, magic, rec_crc32, and always-on img_len geometry. Fills *rec on
 * success; the ONLY definition of "valid record" in the tree. */
int ob_record_load(ob_boot_record_t *rec);

/* Nonzero once timeout_ms has elapsed between two ob_uptime_ms() readings.
 * Lives here rather than in main.c so the host suite can reach it: main.c
 * needs a transport and is only syntax-checked.
 *
 * Wrap-safe by unsigned subtraction, so it stays correct across the 2^32 ms
 * rollover. timeout_ms == 0 means "disabled" and never elapses — that is the
 * documented meaning of OB_IDLE_TIMEOUT_MS = 0, and it must not degenerate
 * into "expire immediately". */
int ob_idle_elapsed(uint32_t start_ms, uint32_t now_ms, uint32_t timeout_ms);

/* Fold delta_ticks of a free-running counter into a running millisecond
 * total, carrying the sub-millisecond remainder so no ticks are lost across
 * calls. Split out of the ports so this arithmetic exists once and the host
 * suite can test it; a port keeps only the register read and the state. */
void ob_ms_accumulate(uint32_t *ms, uint32_t *rem,
                      uint32_t delta_ticks, uint32_t ticks_per_ms);

#endif /* OB_BOOT_DECISION_H */
