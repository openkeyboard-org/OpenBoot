/* OpenBoot hooks shared by the CH57x and CH59x ports. */
#include "openboot_port.h"
#include "boot_decision.h"
#include "port_ch5xx.h"

/* SysTick is the same block on both families. Its low count word sits at
 * +0x08 even though the full counter is 32-bit on CH57x and 64-bit on CH59x
 * (+0x0C is CNT[63:32] on CH59x but reserved on CH57x — the port header's
 * OB_STK_CNT64 says which). Reading the low word gives a wrap-safe tick
 * delta on either RV32 core.
 *
 * SR bit 0 is CNTIF, the compare-match latch, and it is RW0: writing 0
 * clears it, writing 1 is ignored, and stopping the counter via CTLR does
 * NOT clear it (CH572DS1 / CH592DS1 section 3.4.4). The software-interrupt
 * enable also moved between families — SR[31] on CH57x, CTLR[31] on CH59x —
 * so ob_systick_stop()'s two zero writes cover both layouts. */
#define OB_STK_CTLR (*(volatile uint32_t *)0xE000F000u)
#define OB_STK_SR   (*(volatile uint32_t *)0xE000F004u)
#define OB_STK_CNTL (*(volatile uint32_t *)0xE000F008u)
#define OB_STK_CNTH (*(volatile uint32_t *)0xE000F00Cu)
#define OB_STK_RUN  0x05u               /* STE | STCLK */

/* #if on a missing macro is silently 0 — a future port that forgot the
 * define would skip the high-word clear on a 64-bit counter part. */
#ifndef OB_STK_CNT64
#error "port header must define OB_STK_CNT64 (0: 32-bit SysTick CNT, 1: 64-bit)"
#endif

static uint32_t up_ms, up_rem, up_last;

/* Stop the counter, then clear the latched flag — in that order, so a tick
 * cannot re-latch CNTIF between the two stores — then zero the count so the
 * block is back in its reset state. CMP is deliberately not written: it is
 * 64-bit only on CH59x, applications must program it before enabling the
 * SysTick interrupt anyway (vendor SysTick_Config does), and with CNTIF
 * clear a stale compare value is inert until they do. */
static void ob_systick_stop(void)
{
    OB_STK_CTLR = 0;
    OB_STK_SR   = 0;
    OB_STK_CNTL = 0;
#if OB_STK_CNT64
    OB_STK_CNTH = 0;
#endif
}

static void ob_time_init(void)
{
    ob_systick_stop();
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
    /* Hand the application a reset-state SysTick: stopped, count flag
     * clear, counter zeroed. The flag is the load-bearing part: CNTIF
     * survives CTLR=0 (RW0 — see the register notes above) and is re-armed
     * on every warm pass through the bootloader once ANY earlier app has
     * used SysTick, because STK CMP survives a software reset and
     * ob_time_init's free-running counter reaches the leftover compare
     * value within microseconds (bench-proven on CH592, #18: an app that
     * verified SR clear before soft-resetting still reads CNTIF=1 at its
     * next entry — with the reset-default CMP of 0 the flag never arms).
     * An application that then enables the SysTick interrupt path (e.g.
     * the CH59x BLE library's init) takes the stale interrupt the moment
     * STIE goes high. The PFIC needs no write of its own: its pending
     * latch stays clear while STIE is 0 (bench-read IPR[12]=0 with
     * CNTIF=1). */
    ob_systick_stop();
    ((void (*)(void))(uintptr_t)base)();
    /* Containment, not optimization: if a validated-but-wrong entry word
     * ever returns, park deterministically instead of executing whatever
     * the linker placed after this function. */
    for (;;) {
    }
}

void ob_reset(void)
{
    sys_safe_access_enable();
    R8_RST_WDOG_CTRL |= RB_SOFTWARE_RESET;
    sys_safe_access_disable();
    for (;;) {
    }
}
