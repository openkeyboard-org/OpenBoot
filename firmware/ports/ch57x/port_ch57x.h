/*
 * OpenBoot port: CH570/CH572 ("ch57x").
 *
 * Raw registers + libISP only: this port includes CH572SFR.h and ISP572.h
 * and nothing else from the SDK (no CH57x_common.h, no driver .c files).
 */
#ifndef OPENBOOT_PORT_CH57X_H
#define OPENBOOT_PORT_CH57X_H

#include <stdint.h>
#include "../../../protocol/openboot_protocol.h"

#include "CH572SFR.h"
#include "ISP572.h"

/* injected per variant by the build; family values below depend on them */
#ifndef OB_FLASH_APP_END
#error "build must inject OB_FLASH_APP_END (-DOB_FLASH_APP_END=0x3B000)"
#endif
#ifndef OB_CPU_HZ
#error "build must inject OB_CPU_HZ (-DOB_CPU_HZ=6400000 or 100000000)"
#endif
#ifndef OB_CHIP_FAMILY
#error "build must inject OB_CHIP_FAMILY (-DOB_CHIP_FAMILY=OB_FAMILY_CH570/CH572)"
#endif
#if (OB_CHIP_FAMILY != OB_FAMILY_CH570) && (OB_CHIP_FAMILY != OB_FAMILY_CH572)
#error "port_ch57x only supports OB_FAMILY_CH570 / OB_FAMILY_CH572"
#endif
#if OB_FLASH_APP_END > 0x3B000
#error "OB_FLASH_APP_END overlaps the ch57x boot-record page at 0x3B000"
#endif

/* ---- contract constants ---------------------------------------------- */
#define OB_FLASH_APP_START    OB_APP_BASE
#define OB_FLASH_ERASE_BLOCK  4096u
#define OB_FLASH_WRITE_PAGE   256u
#define OB_ERASED_WORD        0xF3F9BDA9u
#define OB_BOOTREQ_ADDR       OB_BOOTREQ_ADDR_CH57X
#define OB_FEATURES           0u   /* no OB_FEAT_CRC_LIVE: XIP may serve stale
                                    * data after controller writes (F26) */

/* no DataFlash on ch57x: boot record lives in the last code-flash block,
 * outside the app region */
#define OB_RECORD_ADDR        0x0003B000u

/* ---- safe access ------------------------------------------------------ */
/* window auto-closes ~16 sys cycles after SIG2; protected writes must
 * follow immediately; never nest enables. always_inline: callers in
 * .highcode must not call back into flash .text (-Os outlines plain
 * inlines) while flash timing is being reconfigured */
__attribute__((always_inline))
static inline void sys_safe_access_enable(void)
{
    R8_SAFE_ACCESS_SIG = SAFE_ACCESS_SIG1;
    R8_SAFE_ACCESS_SIG = SAFE_ACCESS_SIG2;
    __asm__ volatile ("nop\n\tnop" ::: "memory");
}

__attribute__((always_inline))
static inline void sys_safe_access_disable(void)
{
    R8_SAFE_ACCESS_SIG = SAFE_ACCESS_SIG0;
    __asm__ volatile ("" ::: "memory");
}

/* ---- HSE crystal trim -------------------------------------------------- */
/* Load-capacitance field of R8_XT32M_TUNE, encoded as CH57x_clk.c's
 * HSECapTypeDef: value N selects 2N + 6 pF, so 6 = 18 pF — what every CH570
 * EVT example passes as HSECap_18p.
 *
 * This is a BOARD property (crystal spec plus stray PCB capacitance), not a
 * chip constant, so a board whose crystal wants a different load may define
 * OB_HSE_CAP_LOAD itself. 18 pF is the vendor reference value and the only
 * one with EVT precedent. */
#ifndef OB_HSE_CAP_LOAD
#define OB_HSE_CAP_LOAD 6u              /* 18 pF */
#endif
#if OB_HSE_CAP_LOAD > 7u
#error "OB_HSE_CAP_LOAD is a 3-bit field: 0..7 selects 6..20 pF in 2 pF steps"
#endif

/* Register-for-register copy of CH57x_clk.c HSECFG_Capacitance(): preserve
 * the low nibble, write the trim into the high nibble, inside a safe-access
 * window (R8_XT32M_TUNE is a SAM register). */
#define OB_HSE_CAP_INIT() do {                                            \
        uint8_t ob_tune_ = (uint8_t)((R8_XT32M_TUNE & 0x0Fu) |            \
                                     (OB_HSE_CAP_LOAD << 4));             \
        sys_safe_access_enable();                                         \
        R8_XT32M_TUNE = ob_tune_;                                         \
        sys_safe_access_disable();                                        \
    } while (0)

/* ---- USB PHY ----------------------------------------------------------- */
/* PA0/PA1 are shared with the 2-wire debug interface: RB_PIN_DEBUG_EN must
 * be cleared or the PHY never owns the pins — this kills SWD until reset */
#define OB_USB_PHY_ATTACH() do {                                  \
        R16_PIN_ALTERNATE &= (uint16_t)~RB_PIN_DEBUG_EN;          \
        R16_PIN_ALTERNATE |= (RB_PIN_USB_EN | RB_UDP_PU_EN);      \
    } while (0)

#define OB_USB_PHY_DETACH() do {                                  \
        R16_PIN_ALTERNATE &= (uint16_t)~(RB_PIN_USB_EN | RB_UDP_PU_EN); \
    } while (0)

/* ---- UART (single UART, default mapping TX=PA3 RX=PA2) ----------------- */
#define OB_UART_RBR   R8_UART_RBR
#define OB_UART_THR   R8_UART_THR
#define OB_UART_RFC   R8_UART_RFC
#define OB_UART_TFC   R8_UART_TFC
#define OB_UART_FCR   R8_UART_FCR
#define OB_UART_LCR   R8_UART_LCR
#define OB_UART_IER   R8_UART_IER
#define OB_UART_DIV   R8_UART_DIV
#define OB_UART_DL    R16_UART_DL

#define OB_UART_TX_PIN  (1u << 3)   /* PA3 = bTXD_0 */
#define OB_UART_RX_PIN  (1u << 2)   /* PA2 = bRXD_0 */

/* TX must idle high before the direction flip or the first start edge is a
 * glitch; RB_UART_TXD/RB_UART_RXD fields cleared = default PA3/PA2 mapping */
#define OB_UART_PINS_INIT() do {                                          \
        R32_PA_OUT    |= OB_UART_TX_PIN;                                  \
        R32_PA_PD_DRV &= ~(OB_UART_TX_PIN | OB_UART_RX_PIN);              \
        R32_PA_DIR    |= OB_UART_TX_PIN;                                  \
        R32_PA_PU     |= OB_UART_RX_PIN;                                  \
        R32_PA_DIR    &= ~OB_UART_RX_PIN;                                 \
        R16_PIN_ALTERNATE_H &= (uint16_t)~(RB_UART_TXD | RB_UART_RXD);    \
    } while (0)

#endif /* OPENBOOT_PORT_CH57X_H */
