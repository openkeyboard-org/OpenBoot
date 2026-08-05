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
#define OB_APP_RECORD_ADDR \
    ((OPENBOOT_SLOT_BASE) + (OPENBOOT_SLOT_SIZE) - (OPENBOOT_ERASE_BLOCK))

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
    return out->magic == OB_RECORD_MAGIC ? 0 : -1;
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
