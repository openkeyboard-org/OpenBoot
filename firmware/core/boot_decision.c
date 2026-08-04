#include "boot_decision.h"
#include "crc32.h"
#include "ob_xip.h"
#include "openboot_port.h"

uint32_t ob_slot_base(uint32_t slot)
{
    return slot == OB_SLOT_B ? OB_SLOT_B_BASE : OB_SLOT_A_BASE;
}

/* Build-time image room in a slot: everything below its final erase block,
 * which the record owns outright.
 *
 * The record needs a whole block, not just its 32 bytes, because rewriting it
 * means erasing it first - flash only clears bits - and erase granularity is
 * one 4096-byte block. If the image could reach into that block, re-committing
 * a slot would destroy image bytes. Giving the record its own block costs
 * 4 KiB per slot (1.8% on ch592, 3.6% on ch570) and makes every record
 * rewrite safe.
 *
 * Fixed by the build, because the application's linker has to agree with it. */
#define OB_SLOT_IMAGE_MAX (OB_SLOT_SIZE - OB_FLASH_ERASE_BLOCK)

uint32_t ob_slot_record_addr(uint32_t slot)
{
    return ob_slot_base(slot) + OB_SLOT_IMAGE_MAX;
}

uint32_t ob_slot_capacity(uint32_t slot)
{
    /* A wrong-variant image can advertise slots the die does not have: the
     * build constant says 220 KiB per slot while the silicon is half that.
     * Such a slot is unusable WHOLESALE rather than shrunk, because shrinking
     * would move the record and break the address the application was linked
     * against. ob_app_end() is the runtime min(silicon, build). */
    if (ob_slot_base(slot) + OB_SLOT_SIZE > ob_app_end())
        return 0;
    return OB_SLOT_IMAGE_MAX;
}

int ob_record_load(uint32_t slot, ob_boot_record_t *rec)
{
    uint32_t addr = ob_slot_record_addr(slot);
    uint32_t i;
    uint32_t *dst = (uint32_t *)rec;

    if (slot >= OB_SLOT_COUNT)
        return 0;
    /* Reject a slot the silicon cannot hold BEFORE reading it: its record
     * address is a build constant and on a smaller die lands beyond physical
     * flash, so the read itself would be out of bounds. */
    if (ob_slot_capacity(slot) == 0)
        return 0;
    /* Slots live in code flash on both families, so the record reads through
     * XIP like any other app word — no DataFlash path and no port hook. Read
     * as words: OB_XIP_READ32 is the only accessor every port provides, and
     * the record is word-aligned and a whole number of words by construction. */
    for (i = 0; i < OB_BOOT_RECORD_SIZE / 4u; i++)
        dst[i] = OB_XIP_READ32(addr + i * 4u);

    /* The one record-validity definition: magic, record CRC, reserved bytes
     * zero, and image-length geometry — nonzero, 4-aligned, and inside the
     * slot's capacity. Geometry is always-on: a record COMMIT could not have
     * produced must never validate, whichever path asks. */
    if (rec->magic != OB_RECORD_MAGIC)
        return 0;
    if (ob_crc32(rec, OB_RECORD_CRC_LEN) != rec->rec_crc32)
        return 0;
    for (i = 0; i < OB_RECORD_RSVD_BYTES; i++) {
        if (rec->rsvd[i] != 0)
            return 0;
    }
    /* generation 0 is reserved: it is what a wrapped counter or an erased
     * field would produce, and accepting it would let such a record outrank
     * nothing yet still be "valid". Real records start at 1. */
    if (rec->generation == 0)
        return 0;
    return rec->img_len != 0 && (rec->img_len % 4u) == 0 &&
           rec->img_len <= ob_slot_capacity(slot);
}

uint32_t ob_record_store(uint32_t slot, ob_boot_record_t *rec)
{
    uint32_t i;

    if (slot >= OB_SLOT_COUNT)
        return 1;
    rec->magic = OB_RECORD_MAGIC;
    for (i = 0; i < OB_RECORD_RSVD_BYTES; i++)
        rec->rsvd[i] = 0;
    rec->rec_crc32 = ob_crc32(rec, OB_RECORD_CRC_LEN);

    /* Erase the record's own block first: flash only clears bits, so writing
     * over an existing record would AND the two together. The block holds
     * nothing but the record, so this destroys no image bytes, and the OTHER
     * slot's record is untouched throughout - which is what keeps the device
     * bootable across a cut here. */
    if (ob_flash_erase(ob_slot_record_addr(slot), OB_FLASH_ERASE_BLOCK) != 0)
        return 1;
    /* Verify through the controller rather than XIP: on CH57x an XIP read
     * after a controller write can serve stale data for the rest of the
     * power cycle (F26), which would let a failed write look successful. */
    if (ob_flash_write(ob_slot_record_addr(slot), rec, OB_BOOT_RECORD_SIZE) != 0)
        return 1;
    return ob_flash_verify(ob_slot_record_addr(slot), rec, OB_BOOT_RECORD_SIZE);
}

uint32_t ob_next_generation(void)
{
    ob_boot_record_t rec;
    uint32_t slot, best = 0;

    for (slot = 0; slot < OB_SLOT_COUNT; slot++) {
        if (ob_record_load(slot, &rec) && rec.generation > best)
            best = rec.generation;
    }
    /* Saturate rather than wrap: returning 0 would produce a record that
     * ob_record_load rejects, silently bricking further updates. Reaching
     * 2^32 updates is not a real scenario; wrapping into an invalid record
     * would be a real bug. */
    return best == 0xFFFFFFFFu ? 0xFFFFFFFFu : best + 1u;
}

uint32_t ob_boot_select(void)
{
    ob_boot_record_t rec;
    uint32_t slot, best_gen = 0, best = OB_SLOT_NONE;

    /* Highest generation among slots that fully validate. A slot whose record
     * is newer but whose image fails is skipped, not fatal: that is exactly
     * the interrupted-update case, and the older slot must still boot. */
    for (slot = 0; slot < OB_SLOT_COUNT; slot++) {
        if (!ob_record_load(slot, &rec))
            continue;
        if (best != OB_SLOT_NONE && rec.generation <= best_gen)
            continue;
        if (!ob_boot_app_valid(slot))
            continue;
        best_gen = rec.generation;
        best = slot;
    }
    return best;
}

int ob_idle_elapsed(uint32_t start_ms, uint32_t now_ms, uint32_t timeout_ms)
{
    if (timeout_ms == 0u)
        return 0;                       /* 0 disables idle auto-boot */
    return (uint32_t)(now_ms - start_ms) >= timeout_ms;
}

void ob_ms_accumulate(uint32_t *ms, uint32_t *rem,
                      uint32_t delta_ticks, uint32_t ticks_per_ms)
{
    /* Divide first and carry the remainder separately: adding delta_ticks
     * into *rem before dividing would overflow uint32_t on a large delta.
     * Both remainders are < ticks_per_ms, so their sum cannot. */
    *ms  += delta_ticks / ticks_per_ms;
    *rem += delta_ticks % ticks_per_ms;
    if (*rem >= ticks_per_ms) {
        *rem -= ticks_per_ms;
        (*ms)++;
    }
}

int ob_boot_app_valid(uint32_t slot)
{
    ob_boot_record_t rec;
    uint32_t base;

    if (!ob_record_load(slot, &rec))
        return 0;
    base = ob_slot_base(slot);
#ifdef OB_BOOT_IMAGE_CRC
    /* Full image attestation at boot (opt-in: ~0.5 s worst case at 6.4 MHz). */
    if (ob_xip_crc32(base, rec.img_len) != rec.img_crc32)
        return 0;
#endif
    return OB_XIP_READ32(base) != OB_ERASED_WORD;
}

void ob_boot_decide(void)
{
    /* App-requested entry: magic word at reserved top-of-RAM. Read and clear
     * it before evaluating any boot decision because the request is one-shot.
     * A strap or invalid application may still keep us in the bootloader, but
     * must not leave a stale request that swallows the next BOOT. */
    volatile uint32_t *req = (volatile uint32_t *)(uintptr_t)(OB_BOOTREQ_ADDR);
    int requested = (*req == OB_BOOTREQ_MAGIC);
    uint32_t slot;

    if (requested)
        *req = 0;

    /* Strap pin: 10 ms + 5 ms double-read debounce. */
    if (ob_bootpin_asserted()) {
        ob_delay_us(10000);
        if (ob_bootpin_asserted()) {
            ob_delay_us(5000);
            if (ob_bootpin_asserted())
                return;
        }
    }

    if (requested)
        return;

    slot = ob_boot_select();
    if (slot == OB_SLOT_NONE)
        return;
    ob_jump_app(ob_slot_base(slot));
}
