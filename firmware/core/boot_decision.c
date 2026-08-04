#include "boot_decision.h"
#include "crc32.h"
#include "ob_xip.h"
#include "openboot_port.h"

int ob_record_load(ob_boot_record_t *rec)
{
    /* The one record-validity definition (PROTOCOL.md section 9.1): read,
     * magic, record CRC, and image-length geometry — nonzero, 4-aligned,
     * inside the app region. Geometry is always-on: a record that COMMIT
     * could not have produced must never validate, whichever path asks. */
    return ob_record_read(rec) == 0 &&
           rec->magic == OB_RECORD_MAGIC &&
           ob_crc32(rec, 12) == rec->rec_crc32 &&
           rec->img_len != 0 && (rec->img_len % 4u) == 0 &&
           rec->img_len <= ob_app_end() - OB_FLASH_APP_START;
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

int ob_boot_app_valid(void)
{
    ob_boot_record_t rec;

    if (!ob_record_load(&rec))
        return 0;
#ifdef OB_BOOT_IMAGE_CRC
    /* Full image attestation at boot (opt-in: ~0.5 s worst case at 6.4 MHz). */
    if (ob_xip_crc32(OB_FLASH_APP_START, rec.img_len) != rec.img_crc32)
        return 0;
#endif
    return OB_XIP_READ32(OB_FLASH_APP_START) != OB_ERASED_WORD;
}

void ob_boot_decide(void)
{
    /* App-requested entry: magic word at reserved top-of-RAM. Read and clear
     * it before evaluating any boot decision because the request is one-shot.
     * A strap or invalid application may still keep us in the bootloader, but
     * must not leave a stale request that swallows the next BOOT. */
    volatile uint32_t *req = (volatile uint32_t *)(uintptr_t)(OB_BOOTREQ_ADDR);
    int requested = (*req == OB_BOOTREQ_MAGIC);

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

    if (!ob_boot_app_valid())
        return;
    ob_jump_app();
}
