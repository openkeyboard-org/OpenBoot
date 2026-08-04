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
 * record AND image both validate, or OB_SLOT_NONE. This is the one place that
 * decides, and it has no side effects. */
uint32_t ob_boot_select(void);

/* Record + image checks for ONE slot (no strap/bootreq, no side effects).
 * Used by ob_boot_select, the pre-HELLO idle timeout and explicit BOOT. */
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

/* One past the highest generation currently stored in any valid slot record,
 * i.e. the generation a new commit must claim to win. Starts at 1. */
uint32_t ob_next_generation(void);

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
