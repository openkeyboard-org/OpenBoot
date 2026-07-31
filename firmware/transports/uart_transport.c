/*
 * OpenBoot UART transport: fixed 115200 8N1, polled FIFOs, no interrupts.
 *
 * RX runs a byte-at-a-time deframer below the tr_* seam: hunt the
 * 0xB0 0x07 start-of-frame, collect header + payload + CRC into a
 * 4-byte-aligned buffer, and hand the candidate frame to the core. The
 * parser only checks the len byte against OB_MAX_PAYLOAD (silent drop +
 * re-hunt); ALL other validation, including CRC, happens in the core.
 * A >OB_UART_INTERBYTE_MS mid-frame gap (counted in tr_poll invocations,
 * the only time source) drops the partial frame and re-hunts — it never
 * sends a response and never touches the session.
 *
 * The port header supplies the OB_UART_* register aliases (CH57x:
 * R8_UART_*, CH59x: R8_UART1_* — same offsets, different names) and
 * OB_UART_PINS_INIT(); the shared RB_* bit names below are identical in
 * both SFR headers.
 */
#include "openboot_transport.h"
#include "openboot_port.h"

/* Parser states. */
#define ST_HUNT1   0u   /* waiting for SOF1 (0xB0)          */
#define ST_HUNT2   1u   /* waiting for SOF2 (0x07)          */
#define ST_COLLECT 2u   /* collecting header/payload/CRC    */

/* Mid-frame inter-byte timeout expressed in tr_poll invocations. */
#define UART_IDLE_POLLS ((OB_UART_INTERBYTE_MS * 1000u) / OB_POLL_INTERVAL_US)

/* FCR init per the EVT IAP: RX trigger 4 bytes, TX+RX FIFO reset, FIFO
 * enable. CH572SFR.h omits the FIFO-clear bit names, so numeric values
 * are used; the FCR layout is identical on both families (CH592SFR.h:
 * RB_FCR_TX_FIFO_CLR=0x04, RB_FCR_RX_FIFO_CLR=0x02, RB_FCR_FIFO_EN=0x01). */
#define UART_FCR_INIT ((2u << 6) | 0x04u | 0x02u | 0x01u)

static uint8_t  rx_buf[OB_FRAME_OVERHEAD + OB_MAX_PAYLOAD] __attribute__((aligned(4)));
static uint8_t  rx_state;
static uint8_t  rx_pos;    /* bytes collected into rx_buf              */
static uint8_t  rx_need;   /* total frame bytes expected (4, then 8+N) */
static uint16_t rx_idle;   /* consecutive empty polls while mid-frame  */

void tr_init(void)
{
    OB_UART_PINS_INIT();
    OB_UART_FCR = UART_FCR_INIT;
    OB_UART_LCR = RB_LCR_WORD_SZ;    /* 8 data bits, no parity, 1 stop */
    /* DL = round(OB_CPU_HZ / 8 / baud) with DIV (pre-divisor) = 1;
     * folds to a constant: 7 at 6.4 MHz, 65 at 60 MHz. */
    OB_UART_DL  = (uint16_t)(((10u * OB_CPU_HZ / 8u) / OB_UART_BAUD + 5u) / 10u);
    OB_UART_DIV = 1;
    /* RB_IER_TXD_EN gates the TXD pin driver on these chips (it is not
     * an interrupt enable); no RX/TX interrupts are ever enabled. */
    OB_UART_IER = RB_IER_TXD_EN;
}

const uint8_t *tr_poll(uint32_t *avail)
{
    uint32_t n = OB_UART_RFC;

    if (n == 0) {
        if (rx_state != ST_HUNT1 && ++rx_idle >= UART_IDLE_POLLS) {
            rx_state = ST_HUNT1;         /* drop partial frame, re-hunt */
            rx_idle  = 0;
        }
        return 0;
    }
    rx_idle = 0;

    do {
        uint8_t b = OB_UART_RBR;

        switch (rx_state) {
        case ST_HUNT1:
            if (b == OB_UART_SOF1)
                rx_state = ST_HUNT2;
            break;
        case ST_HUNT2:
            if (b == OB_UART_SOF2) {
                rx_state = ST_COLLECT;
                rx_pos   = 0;
                rx_need  = OB_FRAME_HDR_LEN;
            } else if (b != OB_UART_SOF1) {
                /* a repeated 0xB0 stays armed: B0 B0 07 must still lock */
                rx_state = ST_HUNT1;
            }
            break;
        default: /* ST_COLLECT */
            rx_buf[rx_pos++] = b;
            if (rx_pos == OB_FRAME_HDR_LEN && rx_need == OB_FRAME_HDR_LEN) {
                if (rx_buf[2] > OB_MAX_PAYLOAD) {  /* silent drop */
                    rx_state = ST_HUNT1;
                    break;
                }
                rx_need = (uint8_t)(OB_FRAME_OVERHEAD + rx_buf[2]);
            }
            if (rx_pos == rx_need) {
                rx_state = ST_HUNT1;
                *avail   = rx_need;
                /* Any bytes still in the FIFO wait for the next poll
                 * (strict ping-pong: there should be none). */
                return rx_buf;
            }
            break;
        }
    } while (--n);

    return 0;
}

static void uart_put(uint8_t b)
{
    while (OB_UART_TFC >= UART_FIFO_SIZE) {
    }
    OB_UART_THR = b;
}

void tr_send(const uint8_t *frame, uint32_t len)
{
    uart_put(OB_UART_SOF1);
    uart_put(OB_UART_SOF2);
    while (len--)
        uart_put(*frame++);
}

void tr_deinit(void)
{
    while (OB_UART_TFC != 0) {
    }
    ob_delay_us(100);   /* last byte in the shifter: one char at 115200 is ~87 us */
}
