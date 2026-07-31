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
#define OB_APP_RECORD_ADDR   0x0003B000u   /* last code-flash block */

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
#define OB_APP_RECORD_ADDR   0x00007000u   /* last DataFlash block */

#else
#error "define OPENBOOT_CHIP_CH57X or OPENBOOT_CHIP_CH59X"
#endif

int openboot_get_record(ob_boot_record_t *out)
{
    /* ROM API needs a 4-aligned RAM buffer; also serves the XIP copy */
    uint32_t buf[sizeof(*out) / 4u];
    uint32_t *dst = (uint32_t *)out;
    uint32_t i;

#if defined(OPENBOOT_CHIP_CH57X)
    {
        const volatile uint32_t *src = (const volatile uint32_t *)OB_APP_RECORD_ADDR;
        for (i = 0; i < sizeof(buf) / 4u; i++) {
            buf[i] = src[i];
        }
    }
#else
    if (FLASH_EEPROM_CMD(CMD_EEPROM_READ, OB_APP_RECORD_ADDR, buf, sizeof(buf)) != 0) {
        return -1;
    }
#endif

    if (buf[0] != OB_RECORD_MAGIC) {
        return -1;
    }
    for (i = 0; i < sizeof(buf) / 4u; i++) {
        dst[i] = buf[i];
    }
    return 0;
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
