/*
 * OpenBoot port hooks: CH591/CH592.
 */
#include "openboot_port.h"
#include "../port_ch5xx.h"
#include "../flash_ch5xx.h"

/* ---- clock ------------------------------------------------------------ */
/* Runs from RAM: reconfigures flash timing (R8_FLASH_CFG) while XIP would
 * otherwise be fetching this very code. */
/* noinline: LTO must never absorb this into a flash-resident
 * caller — check_highcode --ram-symbol pins it. */
__attribute__((section(".highcode"), noinline))
void ob_family_clock_init(void)
{
#if OB_CPU_HZ == 60000000
    /* CH592 EVT USB_IAP mySetSysClock, verbatim: 480 MHz PLL / 8 = 60 MHz */
    sys_safe_access_enable();
    R8_PLL_CONFIG &= ~(1u << 5);
    sys_safe_access_disable();

    sys_safe_access_enable();
    R32_CLK_SYS_CFG = (1u << 6) | (0x48u & 0x1fu) | RB_TX_32M_PWR_EN | RB_PLL_PWR_EN;
    __asm__ volatile ("nop\n\tnop\n\tnop\n\tnop");
    sys_safe_access_disable();

    /* flash timing for 60 MHz sysclk */
    sys_safe_access_enable();
    R8_FLASH_CFG = 0x52;
    sys_safe_access_disable();

    sys_safe_access_enable();
    R8_PLL_CONFIG |= 1u << 7;
    sys_safe_access_disable();
#elif OB_CPU_HZ == 6400000
    /* boot clock (32 MHz XT / 5); 115200 baud works without any init */
#else
#error "OB_CPU_HZ must be 6400000 or 60000000 on ch59x"
#endif
}

/* ---- boot strap pin (active low) -------------------------------------- */
/* used+noinline: check_board_policy.py proves strap policy by this SYMBOL's
 * presence and size in the linked ELF; LTO must not inline it away. */
__attribute__((used, noinline))
int ob_bootpin_asserted(void)
{
#ifndef OB_BOOT_PIN_MASK
    return 0;
#else
    /* input + pull-up; pull-down/drive bit must be off for the pull-up to
     * win; caller debounces (repeat calls with delay) */
#if OB_BOOT_PIN_PORT_B
    R32_PB_PD_DRV &= ~(uint32_t)OB_BOOT_PIN_MASK;
    R32_PB_PU    |= (uint32_t)OB_BOOT_PIN_MASK;
    R32_PB_DIR   &= ~(uint32_t)OB_BOOT_PIN_MASK;
    return (R32_PB_PIN & (uint32_t)OB_BOOT_PIN_MASK) == 0;
#else
    R32_PA_PD_DRV &= ~(uint32_t)OB_BOOT_PIN_MASK;
    R32_PA_PU    |= (uint32_t)OB_BOOT_PIN_MASK;
    R32_PA_DIR   &= ~(uint32_t)OB_BOOT_PIN_MASK;
    return (R32_PA_PIN & (uint32_t)OB_BOOT_PIN_MASK) == 0;
#endif
#endif
}

/* ---- app-region clamp -------------------------------------------------- */
/* This is the case that matters: CH591 is a 192 KiB part and CH592 a 448 KiB
 * one, and they share this port. Flashing the ch592 image onto a CH591 would
 * otherwise advertise app [0x2000, 0x70000) on a die that ends at 0x30000, and
 * the host would erase and write a range that does not exist. */
#define OB_CHIP_ID_CH591  0x91u
#define OB_CHIP_ID_CH592  0x92u
#define OB_APP_END_CH591  0x00030000u   /* 192 KiB */
#define OB_APP_END_CH592  0x00070000u   /* 448 KiB */

uint32_t ob_app_end(void)
{
    uint32_t silicon;

    switch (R8_CHIP_ID) {
    case OB_CHIP_ID_CH591:
        silicon = OB_APP_END_CH591;
        break;
    case OB_CHIP_ID_CH592:
        silicon = OB_APP_END_CH592;
        break;
    default:
        return OB_FLASH_APP_END;        /* unknown: trust the build */
    }
    return silicon < OB_FLASH_APP_END ? silicon : OB_FLASH_APP_END;
}

void ob_family_read_uid(uint32_t buf[4])
{
    ob_ch5xx_flash_uid_read(buf);
}
