/* Reset-time boot decision: runs before transport init, decides whether to
 * stay in the bootloader or jump to the app. */
#ifndef OB_BOOT_DECISION_H
#define OB_BOOT_DECISION_H

#include "../../protocol/openboot_protocol.h"

/* Slot identifiers. Two slots, A first; see docs/AB-UPDATE.md. */
#define OB_SLOT_A 0u
#define OB_SLOT_B 1u
#define OB_SLOT_COUNT 2u
#define OB_SLOT_NONE 0xFFFFFFFFu

/* Base address of a slot, and where that slot's record sits inside it. */
uint32_t ob_slot_base(uint32_t slot);
uint32_t ob_slot_record_addr(uint32_t slot);

/* Largest image this slot can hold, or 0 when the silicon is too small to
 * contain the slot at all — see the note in boot_decision.c. */
uint32_t ob_slot_capacity(uint32_t slot);

/* Returns only when staying in the bootloader; otherwise calls
 * ob_jump_app(). The RAM boot-request magic is read and cleared first, so it
 * is always consumed. Decision priority is then: strap pin (debounced), boot
 * request, then the newest bootable slot. */
void ob_boot_decide(void);

/* The slot the device should boot: the highest `generation` among slots whose
 * record AND image both validate, or OB_SLOT_NONE. When highest_generation is
 * non-NULL, it receives the highest generation in any valid record, including
 * one whose image is not bootable; COMMIT must outrank all such records. */
uint32_t ob_boot_select(uint32_t *highest_generation);

/* Record + image checks for ONE slot (no strap/bootreq, no side effects).
 * Sole caller: boot_record_trusted's OB_REC_INVALID branch (explicit BOOT
 * after this session invalidated the write slot), which must ask only the
 * untouched slot and so cannot go through ob_boot_select. */
int ob_boot_app_valid(uint32_t slot);

/* The canonical record-validity check: read, magic, rec_crc32, reserved-zero,
 * and img_len geometry against the slot capacity. Fills *rec on success; the
 * ONLY definition of "valid record" in the tree. */
int ob_record_load(uint32_t slot, ob_boot_record_t *rec);

/* Fill in magic, reserved and rec_crc32 for a record the caller has populated
 * with generation/img_len/img_crc32, then write it into its slot and verify.
 * Returns 0 on success. This is the commit point: until it completes, the
 * other slot's record is still the newest valid one. */
uint32_t ob_record_store(uint32_t slot, ob_boot_record_t *rec);

/* Nonzero once `timeout_ticks` have elapsed between two ob_now_ticks()
 * readings. Lives here rather than in main.c so the host suite can reach it:
 * main.c needs a transport and is only syntax-checked.
 *
 * The unsigned (now - start) subtraction is wrap-correct, but that alone is
 * NOT sufficient. The caller must also: (1) keep `timeout_ticks` below 2^32
 * with margin, and (2) sample often enough that no gap between readings steps
 * the delta past the whole [timeout_ticks, 2^32) window — otherwise the
 * deadline is missed until the next full counter wrap. main.c satisfies both:
 * it caps the deadline at 2^31 and reads every loop pass. timeout_ticks == 0
 * means "disabled" and never elapses — the documented meaning of
 * OB_IDLE_TIMEOUT_MS = 0, which must not degenerate into "expire
 * immediately". */
int ob_idle_elapsed(uint32_t start_ticks, uint32_t now_ticks,
                    uint32_t timeout_ticks);

#endif /* OB_BOOT_DECISION_H */
