/* Shared XIP access: the OB_XIP_READ32 override hook (the host harness's
 * port header replaces it to simulate F26 stale reads) and the XIP CRC
 * loop used by the CRC command, COMMIT's coherent paths, and the optional
 * boot-time image check. Header-static on purpose: default builds
 * instantiate exactly one copy (the boot_decision use is compiled only
 * under OB_BOOT_IMAGE_CRC). */
#ifndef OB_XIP_H
#define OB_XIP_H

#include <stdint.h>

#include "crc32.h"
#include "openboot_port.h"   /* port header may define OB_XIP_READ32 */

#ifndef OB_XIP_READ32
#define OB_XIP_READ32(a) (*(const uint32_t *)(uintptr_t)(a))
#endif

/* CRC-32 over [addr, addr+len) as seen through XIP. addr and len must be
 * 4-aligned (callers validate before calling). */
static inline uint32_t ob_xip_crc32(uint32_t addr, uint32_t len)
{
    uint32_t c = ob_crc32_init();
    uint32_t a;

    for (a = addr; a < addr + len; a += 4) {
        uint32_t v = OB_XIP_READ32(a);
        uint8_t w[4];

        w[0] = (uint8_t)v;
        w[1] = (uint8_t)(v >> 8);
        w[2] = (uint8_t)(v >> 16);
        w[3] = (uint8_t)(v >> 24);
        c = ob_crc32_update(c, w, 4);
    }
    return ob_crc32_final(c);
}

#endif /* OB_XIP_H */
