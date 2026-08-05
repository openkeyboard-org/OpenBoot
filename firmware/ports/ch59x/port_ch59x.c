/*
 * OpenBoot port hooks: CH591/CH592.
 */
#include "openboot_port.h"
#include "boot_decision.h"   /* ob_ms_accumulate: shared tick arithmetic */

static void ob_time_init(void);

/* ---- clock ------------------------------------------------------------ */
/* Runs from RAM: reconfigures flash timing (R8_FLASH_CFG) while XIP would
 * otherwise be fetching this very code. */
__attribute__((section(".highcode")))
void ob_port_init(void)
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

/* ---- boot record (DataFlash offset 0x7000, last 4 K block) ------------ */
/* All DataFlash access goes through FLASH_EEPROM_CMD; the ISP592.h
 * EEPROM_ERASE inline is never used (hangs CH592A on partial lengths) and
 * erase length is ALWAYS the full 4096-byte block for the same reason. */
int ob_record_read(ob_boot_record_t *rec)
{
    /* ROM API needs a 4-aligned RAM buffer */
    uint32_t buf[sizeof(*rec) / 4u];
    uint32_t *dst = (uint32_t *)rec;
    uint32_t i;

    if (FLASH_EEPROM_CMD(CMD_EEPROM_READ, OB_RECORD_ADDR, buf, sizeof(buf)) != 0) {
        return -1;
    }
    for (i = 0; i < sizeof(buf) / 4u; i++) {
        dst[i] = buf[i];
    }
    return 0;
}

uint32_t ob_record_write(const ob_boot_record_t *rec)
{
    uint32_t buf[sizeof(*rec) / 4u];
    uint32_t chk[sizeof(*rec) / 4u];
    const uint32_t *src = (const uint32_t *)rec;
    uint32_t i, rc;

    for (i = 0; i < sizeof(buf) / 4u; i++) {
        buf[i] = src[i];
    }

    rc = FLASH_EEPROM_CMD(CMD_EEPROM_ERASE, OB_RECORD_ADDR, NULL, OB_FLASH_ERASE_BLOCK);
    if (rc != 0) {
        return rc;
    }
    rc = FLASH_EEPROM_CMD(CMD_EEPROM_WRITE, OB_RECORD_ADDR, buf, sizeof(buf));
    if (rc != 0) {
        return rc;
    }
    /* read-back verify (controller read, not XIP) */
    rc = FLASH_EEPROM_CMD(CMD_EEPROM_READ, OB_RECORD_ADDR, chk, sizeof(chk));
    if (rc != 0) {
        return rc;
    }
    for (i = 0; i < sizeof(buf) / 4u; i++) {
        if (chk[i] != buf[i]) {
            return 1;
        }
    }
    return 0;
}

uint32_t ob_record_invalidate(void)
{
    return FLASH_EEPROM_CMD(CMD_EEPROM_ERASE, OB_RECORD_ADDR, NULL, OB_FLASH_ERASE_BLOCK);
}

/* ---- boot strap pin (active low) -------------------------------------- */
int ob_bootpin_asserted(void)
{
#ifndef OB_BOOT_PIN_MASK
    return 0;
#else
    /* input + pull-up; pull-down/drive bit must be off for the pull-up to
     * win; caller debounces (repeat calls with delay) */
#if defined(OB_BOOT_PIN_PORT_B) && OB_BOOT_PIN_PORT_B
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
    for (i = 0; i < 8; i++) {
        uid[i] = b[i];
    }
}

/* ---- time -------------------------------------------------------------- */
/* SysTick as a free-running HCLK up-counter — the bootloader's only real
 * clock. Register map and bit semantics from CH592DS1 section 3.4.4; the
 * ch57x port carries the same block, verified against CH572DS1 3.4.4.
 *
 *   CTLR bit0 STE   1 = counter enabled
 *        bit1 STIE  0 = no interrupt (nothing is routed to the PFIC)
 *        bit2 STCLK 1 = HCLK.  0 = HCLK/8, and 0 is the RESET VALUE, so
 *                   enabling with STE alone silently gives an 8x-slow clock
 *        bit3 STRE  0 = keep counting past CMP instead of auto-reloading
 *        bit4 MODE  0 = count up
 *
 * CNT is 64-bit here and 32-bit on ch57x, but its low word sits at +0x08 on
 * both and a 64-bit read cannot be atomic on RV32 — so both ports read the
 * low 32 bits and rely on wrap-safe unsigned deltas. It wraps every ~71 s at
 * 60 MHz (~43 s at the ch57x 100 MHz), far more often than the ~49-day
 * millisecond total, which is why the accumulator exists at all. */
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

void ob_jump_app(void)
{
    OB_STK_CTLR = 0;                     /* hand the app a stopped SysTick */
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
