/*
 * OpenBoot application companion: boot-record access + IAP entry request.
 *
 * Raw registers + libISP headers only (no CH5xx_common.h, no SDK driver
 * .c files). Select the chip family with -DOPENBOOT_CHIP_CH57X or
 * -DOPENBOOT_CHIP_CH59X.
 */
#include <stdint.h>
#include "openboot_app.h"

#if defined(OPENBOOT_CHIP_CH57X) && defined(OPENBOOT_CHIP_CH59X)
#error "define only one of OPENBOOT_CHIP_CH57X / OPENBOOT_CHIP_CH59X"
#endif

#if defined(OPENBOOT_CHIP_CH57X)

#include "CH572SFR.h"
#include "ISP572.h"
#define OB_APP_BOOTREQ_ADDR  OB_BOOTREQ_ADDR_CH57X

#elif defined(OPENBOOT_CHIP_CH59X)

#include "CH592SFR.h"
/* ISP592.h needs RV_STATIC_INLINE (normally from core_riscv.h); its
 * EEPROM_ERASE inline must never be called (hangs CH592A on partial
 * lengths) */
#ifndef RV_STATIC_INLINE
#define RV_STATIC_INLINE static inline
#endif
#include "ISP592.h"
#define OB_APP_BOOTREQ_ADDR  OB_BOOTREQ_ADDR_CH59X

#else
#error "define OPENBOOT_CHIP_CH57X or OPENBOOT_CHIP_CH59X"
#endif

/* Which slot this build is linked for. The application is built once per
 * slot (the parts have no flash remap, so an image cannot execute from two
 * bases), and its record sits in the final erase block of its own slot. */
#ifndef OPENBOOT_SLOT_BASE
#error "define OPENBOOT_SLOT_BASE to the base this image is linked at"
#endif
#ifndef OPENBOOT_SLOT_SIZE
#error "define OPENBOOT_SLOT_SIZE to the slot size OpenBoot was built with"
#endif
#ifndef OPENBOOT_ERASE_BLOCK
#define OPENBOOT_ERASE_BLOCK 4096u
#endif
/* Static sanity on the geometry this build claims. What CANNOT be checked
 * here is that these values match the bootloader actually installed - the
 * bootloader derives its geometry at its own build time - so a mismatch
 * still reads the wrong address; these only reject values no bootloader
 * could have produced. */
#if OPENBOOT_ERASE_BLOCK == 0
#error "OPENBOOT_ERASE_BLOCK must be nonzero"
#elif OPENBOOT_SLOT_SIZE <= OPENBOOT_ERASE_BLOCK
#error "OPENBOOT_SLOT_SIZE must exceed one erase block: the record owns the top block"
#elif (OPENBOOT_SLOT_SIZE % OPENBOOT_ERASE_BLOCK) != 0
#error "OPENBOOT_SLOT_SIZE must be a whole number of erase blocks"
#elif (OPENBOOT_SLOT_BASE % OPENBOOT_ERASE_BLOCK) != 0
#error "OPENBOOT_SLOT_BASE must be erase-block aligned"
#endif
#define OB_APP_RECORD_ADDR \
    ((OPENBOOT_SLOT_BASE) + (OPENBOOT_SLOT_SIZE) - (OPENBOOT_ERASE_BLOCK))
#define OB_APP_SLOT_CAPACITY \
    ((OPENBOOT_SLOT_SIZE) - (OPENBOOT_ERASE_BLOCK))

/* CRC-32/ISO-HDLC, bitwise. Self-contained on purpose: the companion is one
 * .c file an application drops in, and a record check has no speed needs -
 * 28 bytes, once. Matches firmware/core/crc32.c and zlib.crc32. */
static uint32_t ob_app_crc32(const void *data, uint32_t len)
{
    const uint8_t *p = (const uint8_t *)data;
    uint32_t crc = 0xFFFFFFFFu;
    uint32_t bit;

    while (len--) {
        crc ^= *p++;
        for (bit = 0; bit < 8; bit++)
            crc = (crc >> 1) ^ (0xEDB88320u & (0u - (crc & 1u)));
    }
    return ~crc;
}

int openboot_record_valid(const ob_boot_record_t *rec)
{
    uint32_t i;

    /* The bootloader's own rule (boot_decision.c ob_record_load), applied
     * field for field: a record the bootloader would refuse to boot must
     * not be reported to the application as good. */
    if (rec->magic != OB_RECORD_MAGIC)
        return 0;
    if (ob_app_crc32(rec, OB_RECORD_CRC_LEN) != rec->rec_crc32)
        return 0;
    for (i = 0; i < OB_RECORD_RSVD_BYTES; i++) {
        if (rec->rsvd[i] != 0)
            return 0;
    }
    if (rec->generation == 0)
        return 0;
    return rec->img_len != 0 && (rec->img_len % 4u) == 0 &&
           rec->img_len <= OB_APP_SLOT_CAPACITY;
}

int openboot_get_record(ob_boot_record_t *out)
{
    /* Both families keep slots - and therefore records - in code flash, so
     * this is a plain memory read on either chip. No ROM API, and CH59x
     * applications no longer need libISP592 linked for this. */
    const volatile uint32_t *src =
        (const volatile uint32_t *)(uintptr_t)OB_APP_RECORD_ADDR;
    uint32_t *dst = (uint32_t *)out;
    uint32_t i;

    for (i = 0; i < sizeof(*out) / 4u; i++) {
        dst[i] = src[i];
    }
    /* Magic alone used to pass here. A record torn by a power cut during
     * COMMIT's write keeps its magic word long before its CRC seals, so
     * magic-only told applications a half-written record was good. */
    return openboot_record_valid(out) ? 0 : -1;
}

void openboot_request_update(void)
{
    *(volatile uint32_t *)OB_APP_BOOTREQ_ADDR = OB_BOOTREQ_MAGIC;

    /* software reset; safe-access window closes ~16 sys cycles after SIG2,
     * so the protected write follows immediately */
    R8_SAFE_ACCESS_SIG = SAFE_ACCESS_SIG1;
    R8_SAFE_ACCESS_SIG = SAFE_ACCESS_SIG2;
    __asm__ volatile ("nop\n\tnop" ::: "memory");
    R8_RST_WDOG_CTRL |= RB_SOFTWARE_RESET;
    R8_SAFE_ACCESS_SIG = SAFE_ACCESS_SIG0;
    for (;;) {
    }
}
