/*
 * OpenBoot port hooks: CH570/CH572.
 */
#include "openboot_port.h"

/* ---- clock ------------------------------------------------------------ */
/* Runs from RAM: reconfigures flash timing (R8_FLASH_CFG/R8_FLASH_SCK)
 * while XIP would otherwise be fetching this very code. */
__attribute__((section(".highcode")))
void ob_port_init(void)
{
#if OB_CPU_HZ == 100000000
    /* CH57x_sys.c SetSysClock PLL branch, 600 MHz PLL / 6 = 100 MHz —
     * CLK_SOURCE_HSE_PLL_100MHz, the value every EVT USB example for this
     * family uses. XT32M is already on: the 6.4 MHz boot clock divides it.
     * 60 MHz (div 10) enumerates but is not electrically stable enough for
     * the ch570 USB PHY; see the 2026-07-29 bench notes. */
    uint32_t i;

    /* Trim the HSE load capacitance BEFORE powering the PLL: the PLL
     * multiplies the 32 MHz crystal, and USB's 48 MHz is derived from the
     * same PLL, so crystal pulling lands directly on a bus whose full-speed
     * tolerance is +/-0.25%. Every CH570 EVT example that brings up the PLL
     * calls HSECFG_Capacitance(HSECap_18p) first; we cannot call it (the
     * StdPeriphDriver is not linked, only libISP), so OB_HSE_CAP_INIT()
     * reproduces it register-for-register. */
    OB_HSE_CAP_INIT();

    sys_safe_access_enable();
    R8_HFCK_PWR_CTRL |= RB_CLK_PLL_PON;
    sys_safe_access_disable();
    for (i = 0; i < 2000; i++) {
        __asm__ volatile ("nop");
    }

    /* flash timing must be set before the sysclk mux switches to PLL */
    sys_safe_access_enable();
    R8_FLASH_CFG = 0x01;
    R8_FLASH_SCK |= 1u << 4;
    sys_safe_access_disable();

    sys_safe_access_enable();
    R8_SLP_POWER_CTRL |= 0x40;
    R8_CLK_SYS_CFG = 0x46;      /* mode 01 (PLL) | div 6 = 100 MHz */
    sys_safe_access_disable();
#elif OB_CPU_HZ == 6400000
    /* boot clock (32 MHz XT / 5); 115200 baud works without any init */
#else
#error "OB_CPU_HZ must be 6400000 or 100000000 on ch57x"
#endif
}

/* ---- flash ------------------------------------------------------------ */
/* No range checks here — the core validates against the app region first;
 * the ROM API would happily erase the bootloader. */
uint32_t ob_flash_erase(uint32_t addr, uint32_t len)
{
    return FLASH_ROM_ERASE(addr, len);
}

uint32_t ob_flash_write(uint32_t addr, const void *buf, uint32_t len)
{
    return FLASH_ROM_WRITE(addr, (void *)(uintptr_t)buf, len);
}

uint32_t ob_flash_verify(uint32_t addr, const void *buf, uint32_t len)
{
    return FLASH_ROM_VERIFY(addr, (void *)(uintptr_t)buf, len);
}

/* ---- boot record (code flash 0x3B000; no DataFlash on ch57x) ---------- */
/* XIP read: valid at boot, but stale after controller writes this power
 * cycle (F26) — callers must not re-read right after ob_record_write;
 * the write path verifies via the controller instead. */
int ob_record_read(ob_boot_record_t *rec)
{
    const volatile uint32_t *src = (const volatile uint32_t *)OB_RECORD_ADDR;
    uint32_t *dst = (uint32_t *)rec;
    uint32_t i;

    for (i = 0; i < sizeof(*rec) / 4u; i++) {
        dst[i] = src[i];
    }
    return 0;
}

uint32_t ob_record_write(const ob_boot_record_t *rec)
{
    /* ROM API needs a 4-aligned RAM buffer */
    uint32_t buf[sizeof(*rec) / 4u];
    const uint32_t *src = (const uint32_t *)rec;
    uint32_t i, rc;

    for (i = 0; i < sizeof(buf) / 4u; i++) {
        buf[i] = src[i];
    }

    rc = FLASH_ROM_ERASE(OB_RECORD_ADDR, OB_FLASH_ERASE_BLOCK);
    if (rc != 0) {
        return rc;
    }
    rc = FLASH_ROM_WRITE(OB_RECORD_ADDR, buf, sizeof(buf));
    if (rc != 0) {
        return rc;
    }
    /* controller verify, not XIP (F26) */
    return FLASH_ROM_VERIFY(OB_RECORD_ADDR, buf, sizeof(buf));
}

uint32_t ob_record_invalidate(void)
{
    return FLASH_ROM_ERASE(OB_RECORD_ADDR, OB_FLASH_ERASE_BLOCK);
}

/* ---- boot strap pin (active low) -------------------------------------- */
#if defined(OB_BOOT_PIN_MASK) && defined(OB_BOOT_PIN_PORT_B) && OB_BOOT_PIN_PORT_B
#error "ch57x has no GPIO port B"
#endif

int ob_bootpin_asserted(void)
{
#ifndef OB_BOOT_PIN_MASK
    return 0;
#else
    /* input + pull-up; pull-down/drive bit must be off for the pull-up to
     * win; caller debounces (repeat calls with delay) */
    R32_PA_PD_DRV &= ~(uint32_t)OB_BOOT_PIN_MASK;
    R32_PA_PU    |= (uint32_t)OB_BOOT_PIN_MASK;
    R32_PA_DIR   &= ~(uint32_t)OB_BOOT_PIN_MASK;
    return (R32_PA_PIN & (uint32_t)OB_BOOT_PIN_MASK) == 0;
#endif
}

/* ---- app-region clamp -------------------------------------------------- */
/* Both ch57x variants are 240 KiB parts, so the clamp is a no-op today; it
 * exists so the rule ("trust the silicon, not the build") is uniform across
 * ports and so a future variant with less flash is handled by adding one
 * case rather than by remembering this file exists. */
#define OB_CHIP_ID_CH570  0x70u
#define OB_CHIP_ID_CH572  0x72u
#define OB_APP_END_240K   0x0003B000u   /* 240 KiB minus the record block */

uint32_t ob_app_end(void)
{
    uint32_t silicon;

    switch (R8_CHIP_ID) {
    case OB_CHIP_ID_CH570:
    case OB_CHIP_ID_CH572:
        silicon = OB_APP_END_240K;
        break;
    default:
        return OB_FLASH_APP_END;        /* unknown: trust the build */
    }
    return silicon < OB_FLASH_APP_END ? silicon : OB_FLASH_APP_END;
}

/* ---- identity ---------------------------------------------------------- */
uint8_t ob_chip_rev(void)
{
    return R8_CHIP_ID;
}

void ob_read_uid(uint8_t uid[8])
{
    /* 4-aligned RAM buffer; oversized in case the ROM scribbles past 8 */
    uint32_t buf[4] = {0, 0, 0, 0};
    const uint8_t *b = (const uint8_t *)buf;
    uint32_t i;

    FLASH_EEPROM_CMD(CMD_GET_UNIQUE_ID, 0, buf, 0);
    if ((buf[0] | buf[1]) == 0) {
        /* CH570 silicon returns all zeros for CMD_GET_UNIQUE_ID (bench,
         * 2026-07-28). Fall back to the ROM MAC + 2-byte checksum — the
         * SDK's own unique-ID source (ISP572.h GetMACAddress). */
        FLASH_EEPROM_CMD(CMD_GET_ROM_INFO, ROM_CFG_MAC_ADDR, buf, 0);
    }
    for (i = 0; i < 8; i++) {
        uid[i] = b[i];
    }
}

/* ---- misc -------------------------------------------------------------- */
void ob_delay_us(uint32_t us)
{
    /* 2-insn loop (addi+bnez) assumed ~4 CPU cycles/iteration from XIP;
     * coarse on purpose — only used for debounce/settle waits */
    uint32_t n = (us * (OB_CPU_HZ / 100000u)) / 40u;

    if (n == 0) {
        n = 1;
    }
    __asm__ volatile (
        "1: addi %0, %0, -1\n\t"
        "bnez %0, 1b"
        : "+r"(n));
}

void ob_jump_app(void)
{
    ((void (*)(void))OB_FLASH_APP_START)();
    __builtin_unreachable();
}

void ob_reset(void)
{
    sys_safe_access_enable();
    R8_RST_WDOG_CTRL |= RB_SOFTWARE_RESET;
    sys_safe_access_disable();
    for (;;) {
    }
}
