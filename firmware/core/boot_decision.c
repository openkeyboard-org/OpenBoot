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
    /* App-requested entry: magic word at reserved top-of-RAM. Read and
     * clear it BEFORE anything else can return, because the request is
     * one-shot: whichever reason keeps us in the bootloader must consume
     * it, or the stale word sits in RAM and swallows the next BOOT — the
     * host would be told OK and the reset would land back here.
     *
     * Only the strap below can return before the request is examined, and
     * no shipped board configures one (OB_BOOT_PIN_MASK is unset, so
     * ob_bootpin_asserted() is a `return 0` stub — see
     * boards/generic-ch5?x.mk and `make board-policy`). The ordering is
     * kept because the knob is still supported for a product board that
     * wants a hold-a-key update mode, and it costs nothing. */
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
