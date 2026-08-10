/*
 * OpenBoot port: CH591/CH592 ("ch59x").
 *
 * Raw registers + libISP only: this port includes CH592SFR.h and ISP592.h
 * and nothing else from the SDK (no CH59x_common.h, no driver .c files).
 */
#ifndef OPENBOOT_PORT_CH59X_H
#define OPENBOOT_PORT_CH59X_H

#include <stdint.h>
#include "../../../protocol/openboot_protocol.h"

#include "CH592SFR.h"

/* ISP592.h needs RV_STATIC_INLINE (normally from core_riscv.h, which we do
 * not include); its EEPROM_ERASE inline must NEVER be called — it spins
 * forever on CH592A when Length % 4096 != 0 */
#ifndef RV_STATIC_INLINE
#define RV_STATIC_INLINE static inline
#endif
#include "ISP592.h"

/* injected per variant by the build; family values below depend on them */
#ifndef OB_FLASH_APP_END
#error "build must inject OB_FLASH_APP_END (-DOB_FLASH_APP_END=0x30000/0x70000)"
#endif
#ifndef OB_CPU_HZ
#error "build must inject OB_CPU_HZ (-DOB_CPU_HZ=6400000 or 60000000)"
#endif
#ifndef OB_CHIP_FAMILY
#error "build must inject OB_CHIP_FAMILY (-DOB_CHIP_FAMILY=OB_FAMILY_CH591/CH592)"
#endif
#if (OB_CHIP_FAMILY != OB_FAMILY_CH591) && (OB_CHIP_FAMILY != OB_FAMILY_CH592)
#error "port_ch59x only supports OB_FAMILY_CH591 / OB_FAMILY_CH592"
#endif
#if OB_FLASH_APP_END > 0x70000
#error "OB_FLASH_APP_END exceeds ch59x code flash"
#endif

/* ---- contract constants ---------------------------------------------- */
#define OB_FLASH_APP_START    OB_APP_BASE
#define OB_FLASH_ERASE_BLOCK  4096u
#define OB_FLASH_WRITE_PAGE   256u
#define OB_ERASED_WORD        0xF3F9BDA9u  /* bench-verified on CH592A silicon
                                            * (0xF5F9BDA9 from the EVT lore was
                                            * WRONG: real erased XIP reads the
                                            * same word as CH57x) */
#define OB_BOOTREQ_ADDR       OB_BOOTREQ_ADDR_CH59X
#define OB_STK_CNT64          1    /* 64-bit SysTick counter; CNT[63:32] at +0x0C */
#define OB_FEATURES           OB_FEAT_CRC_LIVE

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

/* ---- USB PHY ----------------------------------------------------------- */
#define OB_USB_PHY_ATTACH() do {                                  \
        R16_PIN_ANALOG_IE |= (RB_PIN_USB_IE | RB_PIN_USB_DP_PU);  \
    } while (0)

#define OB_USB_PHY_DETACH() do {                                  \
        R16_PIN_ANALOG_IE &= (uint16_t)~(RB_PIN_USB_IE | RB_PIN_USB_DP_PU); \
    } while (0)

/* ---- UART (UART1, default mapping TX=PA9 RX=PA8) ----------------------- */
#define OB_UART_RBR   R8_UART1_RBR
#define OB_UART_THR   R8_UART1_THR
#define OB_UART_RFC   R8_UART1_RFC
#define OB_UART_TFC   R8_UART1_TFC
#define OB_UART_FCR   R8_UART1_FCR
#define OB_UART_LCR   R8_UART1_LCR
#define OB_UART_IER   R8_UART1_IER
#define OB_UART_DIV   R8_UART1_DIV
#define OB_UART_DL    R16_UART1_DL

/* TX must idle high before the direction flip or the first start edge is a
 * glitch. OB_UART1_REMAP (board knob) selects the RB_PIN_UART1 alternate
 * mapping RXD1_/TXD1_ on PB12/PB13; default is PA8/PA9. */
#if OB_UART1_REMAP

#define OB_UART_TX_PIN  (1u << 13)  /* PB13 = TXD1_ */
#define OB_UART_RX_PIN  (1u << 12)  /* PB12 = RXD1_ */

/* Clear PB12's digital-input-disable bit in R32_PIN_CONFIG2 (PB10..15
 * occupy bits 26..31): the vendor GPIOB_PinCfg(pin, ENABLE) does this as
 * part of standard pin setup and a raw-register port must match it. */
#define OB_UART_PINS_INIT() do {                                          \
        R32_PB_OUT    |= OB_UART_TX_PIN;                                  \
        R32_PB_PD_DRV &= ~(OB_UART_TX_PIN | OB_UART_RX_PIN);              \
        R32_PB_DIR    |= OB_UART_TX_PIN;                                  \
        R32_PB_PU     |= OB_UART_RX_PIN;                                  \
        R32_PB_DIR    &= ~OB_UART_RX_PIN;                                 \
        R32_PIN_CONFIG2 &= ~(1u << 28);                                   \
        R16_PIN_ALTERNATE |= RB_PIN_UART1;                                \
    } while (0)

/* Handoff quiesce: stop driving TX (back to input, its reset state). The
 * RX pull-up and the remap bit are inert to a pin's next owner. */
#define OB_UART_PINS_DEINIT() do {                                        \
        R32_PB_DIR    &= ~OB_UART_TX_PIN;                                 \
    } while (0)

#else /* default mapping */

#define OB_UART_TX_PIN  (1u << 9)   /* PA9 = bTXD1 */
#define OB_UART_RX_PIN  (1u << 8)   /* PA8 = bRXD1 */

/* A default-mapping board may reuse the alternate pins. Undo any application
 * configuration before clearing the remap so PB12/PB13 cannot keep driving. */
#if OB_UART1_ALT_PINS_HIZ
#define OB_UART_ALT_PINS_RELEASE() do {                                   \
        const uint32_t ob_alt_pins = (1u << 12) | (1u << 13);             \
        R32_PB_PD_DRV &= ~ob_alt_pins;                                    \
        R32_PB_PU     &= ~ob_alt_pins;                                    \
        R32_PB_DIR    &= ~ob_alt_pins;                                    \
    } while (0)
#else
#define OB_UART_ALT_PINS_RELEASE() do { } while (0)
#endif

/* Clear PA8's digital-input-disable bit in R32_PIN_CONFIG2 (PA4..15 =
 * bits 4..15), matching the vendor GPIOA_PinCfg(pin, ENABLE) behavior. */
#define OB_UART_PINS_INIT() do {                                          \
        R32_PA_OUT    |= OB_UART_TX_PIN;                                  \
        R32_PA_PD_DRV &= ~(OB_UART_TX_PIN | OB_UART_RX_PIN);              \
        R32_PA_DIR    |= OB_UART_TX_PIN;                                  \
        R32_PA_PU     |= OB_UART_RX_PIN;                                  \
        R32_PA_DIR    &= ~OB_UART_RX_PIN;                                 \
        R32_PIN_CONFIG2 &= ~(1u << 8);                                    \
        OB_UART_ALT_PINS_RELEASE();                                       \
        R16_PIN_ALTERNATE &= (uint16_t)~RB_PIN_UART1;                     \
    } while (0)

/* Handoff quiesce: stop driving TX (back to input, its reset state). */
#define OB_UART_PINS_DEINIT() do {                                        \
        R32_PA_DIR    &= ~OB_UART_TX_PIN;                                 \
    } while (0)

#endif /* OB_UART1_REMAP */

#endif /* OPENBOOT_PORT_CH59X_H */
