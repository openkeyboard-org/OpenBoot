/* OpenBoot hooks shared by the CH57x and CH59x ports. */
#include "openboot_port.h"
#include "boot_decision.h"
#include "port_ch5xx.h"

/* SysTick is the same block on both families. Its low count word sits at
 * +0x08 even though the full counter is 32-bit on CH57x and 64-bit on CH59x.
 * Reading the low word gives a wrap-safe tick delta on either RV32 core. */
#define OB_STK_CTLR (*(volatile uint32_t *)0xE000F000u)
#define OB_STK_CNTL (*(volatile uint32_t *)0xE000F008u)
#define OB_STK_RUN  0x05u               /* STE | STCLK */

static uint32_t up_ms, up_rem, up_last;

static void ob_time_init(void)
{
    OB_STK_CTLR = 0;
    OB_STK_CNTL = 0;
    up_ms = up_rem = up_last = 0;
    OB_STK_CTLR = OB_STK_RUN;
}

void ob_port_init(void)
{
    ob_family_clock_init();
    ob_time_init();
}

/* No range checks here: the core validates before calling the ROM API. */
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

uint8_t ob_chip_rev(void)
{
    return R8_CHIP_ID;
}

void ob_read_uid(uint8_t uid[8])
{
    uint32_t buf[4] = {0, 0, 0, 0};
    const uint8_t *b = (const uint8_t *)buf;
    uint32_t i;

    ob_family_read_uid(buf);
    for (i = 0; i < 8; i++)
        uid[i] = b[i];
}

uint32_t ob_uptime_ms(void)
{
    uint32_t now = OB_STK_CNTL;

    ob_ms_accumulate(&up_ms, &up_rem, now - up_last, OB_CPU_HZ / 1000u);
    up_last = now;
    return up_ms;
}

void ob_delay_us(uint32_t us)
{
    /* Two-instruction loop (addi+bnez), approximately four XIP CPU cycles. */
    uint32_t n = (us * (OB_CPU_HZ / 100000u)) / 40u;

    if (n == 0)
        n = 1;
    __asm__ volatile (
        "1: addi %0, %0, -1\n\t"
        "bnez %0, 1b"
        : "+r"(n));
}

void ob_jump_app(uint32_t base)
{
    OB_STK_CTLR = 0;
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
