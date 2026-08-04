/*
 * OpenBoot port hooks: CH570/CH572.
 */
#include "openboot_port.h"
#include "boot_decision.h"   /* ob_ms_accumulate: shared tick arithmetic */

static void ob_time_init(void);

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
    /* Last, so the tick rate matches the clock we just settled on. */
    ob_time_init();
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

/* ---- time -------------------------------------------------------------- */
/* SysTick as a free-running HCLK up-counter — the bootloader's only real
 * clock. Register map and bit semantics from CH572DS1 section 3.4.4; the
 * ch59x port carries the same block, verified against CH592DS1 3.4.4.
 *
 *   CTLR bit0 STE   1 = counter enabled
 *        bit1 STIE  0 = no interrupt (nothing is routed to the PFIC)
 *        bit2 STCLK 1 = HCLK.  0 = HCLK/8, and 0 is the RESET VALUE, so
 *                   enabling with STE alone silently gives an 8x-slow clock
 *        bit3 STRE  0 = keep counting past CMP instead of auto-reloading
 *        bit4 MODE  0 = count up
 *
 * CNT is 32 bits here (64 on ch59x) but its low word is at +0x08 on both, so
 * both ports read 32 bits and rely on wrap-safe unsigned deltas. At 100 MHz
 * it wraps every ~43 s, far more often than the ~49-day millisecond total,
 * which is why the accumulator exists at all. */
#define OB_STK_CTLR (*(volatile uint32_t *)0xE000F000u)
#define OB_STK_CNTL (*(volatile uint32_t *)0xE000F008u)
#define OB_STK_RUN  0x05u               /* STE | STCLK */

static uint32_t up_ms, up_rem, up_last;

static void ob_time_init(void)
{
    OB_STK_CTLR = 0;                     /* stop before touching the count */
    OB_STK_CNTL = 0;
    up_ms = up_rem = up_last = 0;
    OB_STK_CTLR = OB_STK_RUN;
}

uint32_t ob_uptime_ms(void)
{
    uint32_t now = OB_STK_CNTL;

    ob_ms_accumulate(&up_ms, &up_rem, now - up_last, OB_CPU_HZ / 1000u);
    up_last = now;
    return up_ms;
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

void ob_jump_app(uint32_t base)
{
    OB_STK_CTLR = 0;                     /* hand the app a stopped SysTick */
    ((void (*)(void))(uintptr_t)base)();
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
