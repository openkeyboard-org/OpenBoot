/*
 * OpenBoot port hooks: CH570/CH572.
 */
#include "openboot_port.h"
#include "../port_ch5xx.h"

/* ---- clock ------------------------------------------------------------ */
/* Runs from RAM: reconfigures flash timing (R8_FLASH_CFG/R8_FLASH_SCK)
 * while XIP would otherwise be fetching this very code. */
__attribute__((section(".highcode")))
void ob_family_clock_init(void)
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

/* ---- boot strap pin (active low) -------------------------------------- */
#if defined(OB_BOOT_PIN_MASK) && OB_BOOT_PIN_PORT_B
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
#define OB_APP_END_240K   0x0003C000u   /* all 240 KiB of CodeFlash */

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

/* CH570 returns zero for GET_UNIQUE_ID, so use the ROM MAC fallback there. */
void ob_family_read_uid(uint32_t buf[4])
{
    FLASH_EEPROM_CMD(CMD_GET_UNIQUE_ID, 0, buf, 0);
    if ((buf[0] | buf[1]) == 0) {
        /* CH570 silicon returns all zeros for CMD_GET_UNIQUE_ID (bench,
         * 2026-07-28). Fall back to the ROM MAC + 2-byte checksum — the
         * SDK's own unique-ID source (ISP572.h GetMACAddress). */
        FLASH_EEPROM_CMD(CMD_GET_ROM_INFO, ROM_CFG_MAC_ADDR, buf, 0);
    }
}
