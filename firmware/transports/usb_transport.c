/*
 * OpenBoot USB transport: full-speed HID device, fully polled (USB
 * interrupts are never enabled; RB_UC_INT_BUSY auto-NAKs while an event
 * is pending, so the 20 us poll cadence loses nothing).
 *
 * Identity: VID 0x1209 PID 0x0001 (pid.codes test range), one vendor
 * usage-page HID interface, EP1 interrupt IN+OUT, 64-byte reports, no
 * report IDs. One OBP frame per report, zero-padded; the core does ALL
 * frame validation. iManufacturer and iProduct share one "OpenBoot"
 * string to save flash (a per-family product name would cost ~40 bytes
 * of rodata for no protocol value — HELLO reports the chip family).
 *
 * The USB device register block (0x40008000) is byte-identical on CH57x
 * and CH59x (verified against CH572SFR.h and CH592SFR.h), so the R8_ /
 * R16_ register names are used directly; only the PHY attach/detach
 * differs and comes from the port as OB_USB_PHY_ATTACH / _DETACH().
 *
 * EP1 OUT flow: on a received report the endpoint is switched to NAK and
 * the report flagged; tr_poll hands the DMA buffer to the core and only
 * re-ACKs on the following call, so the buffer stays valid until the
 * next tr_poll/tr_send as the transport contract requires.
 */
#include "boot_core.h"
#include "openboot_transport.h"
#include "openboot_port.h"

/* Standard request codes (local: the bare SFR headers do not name them). */
#define RQ_GET_STATUS        0x00u
#define RQ_CLEAR_FEATURE     0x01u
#define RQ_SET_FEATURE       0x03u
#define RQ_SET_ADDRESS       0x05u
#define RQ_GET_DESCRIPTOR    0x06u
#define RQ_GET_CONFIGURATION 0x08u
#define RQ_SET_CONFIGURATION 0x09u
#define RQ_GET_INTERFACE     0x0Au
/* HID class request codes. */
#define HIDRQ_SET_IDLE       0x0Au

#define REPORT_DESC_LEN 25u

static const uint8_t dev_desc[18] = {
    18,   0x01,             /* bLength, DEVICE                       */
    0x10, 0x01,             /* bcdUSB 1.10                           */
    0x00, 0x00, 0x00,       /* class/subclass/protocol per interface */
    64,                     /* bMaxPacketSize0                       */
    0x09, 0x12, 0x01, 0x00, /* idVendor 0x1209, idProduct 0x0001     */
    (uint8_t)OB_BL_VERSION,
    (uint8_t)(OB_BL_VERSION >> 8), /* bcdDevice = bootloader version */
    1, 2, 3,                /* iManufacturer, iProduct, iSerialNumber */
    1,                      /* bNumConfigurations                    */
};

static const uint8_t cfg_desc[41] = {
    /* configuration: total 41, 1 interface, bus powered, 100 mA */
    9, 0x02, 41, 0, 1, 1, 0, 0x80, 50,
    /* interface 0: HID class (3,0,0), 2 endpoints */
    9, 0x04, 0, 0, 2, 0x03, 0x00, 0x00, 0,
    /* HID: bcdHID 1.11, no country, one report descriptor */
    9, 0x21, 0x11, 0x01, 0x00, 1, 0x22, REPORT_DESC_LEN, 0,
    /* EP1 OUT: interrupt, 64 bytes, 1 ms */
    7, 0x05, 0x01, 0x03, 0x40, 0x00, 1,
    /* EP1 IN: interrupt, 64 bytes, 1 ms */
    7, 0x05, 0x81, 0x03, 0x40, 0x00, 1,
};

static const uint8_t report_desc[REPORT_DESC_LEN] = {
    0x06, 0x00, 0xFF,       /* Usage Page (vendor 0xFF00)  */
    0x09, 0x01,             /* Usage (1)                   */
    0xA1, 0x01,             /* Collection (Application)    */
    0x15, 0x00,             /*   Logical Minimum (0)       */
    0x26, 0xFF, 0x00,       /*   Logical Maximum (255)     */
    0x75, 0x08,             /*   Report Size (8)           */
    0x95, 0x40,             /*   Report Count (64)         */
    0x09, 0x01,             /*   Usage (1)                 */
    0x81, 0x02,             /*   Input (Data,Var,Abs)      */
    0x09, 0x01,             /*   Usage (1)                 */
    0x91, 0x02,             /*   Output (Data,Var,Abs)     */
    0xC0,                   /* End Collection              */
};

static const uint8_t str_lang[4] = { 4, 0x03, 0x09, 0x04 };

/* Serves both string index 1 (manufacturer) and 2 (product). */
static const uint8_t str_openboot[18] = {
    18, 0x03,
    'O', 0, 'p', 0, 'e', 0, 'n', 0, 'B', 0, 'o', 0, 'o', 0, 't', 0,
};

/* String 3: 16 uppercase hex chars of the 8-byte ROM UID, built once at
 * tr_init (UTF-16LE high bytes stay zero from BSS). */
static uint8_t str_serial[2 + 32];

/* Endpoint DMA buffers. EP0: single 64-byte SETUP/IN/OUT buffer (EP4
 * disabled). EP1: RB_UEP1_RX_EN|RB_UEP1_TX_EN with RB_UEP1_BUF_MOD
 * clear = 64-byte OUT buffer at UEP1_DMA followed by the 64-byte IN
 * buffer, per the SFR buffer-mode table and the EVT IAP layout. */
static uint8_t ep0_buf[64]  __attribute__((aligned(4)));
static uint8_t ep1_buf[128] __attribute__((aligned(4)));

static uint8_t setup_req;   /* bRequest awaiting its EP0 IN stage       */
static uint8_t pend_addr;   /* address latched after SET_ADDRESS status */
static uint8_t dev_config;
static uint8_t rx_state;    /* 0 idle, 1 report pending, 2 with core    */
static uint8_t tx_busy;

static void usb_ep_reset(void)
{
    R8_UEP0_CTRL = UEP_R_RES_ACK | UEP_T_RES_NAK;
    R8_UEP1_CTRL = UEP_R_RES_ACK | UEP_T_RES_NAK | RB_UEP_AUTO_TOG;
    rx_state   = 0;
    tx_busy    = 0;
    dev_config = 0;
}

static void ep0_setup(void)
{
    uint8_t        bm   = ep0_buf[0];
    uint8_t        rq   = ep0_buf[1];
    uint8_t        vl   = ep0_buf[2];   /* wValue low  */
    uint8_t        vh   = ep0_buf[3];   /* wValue high */
    uint8_t        ixl  = ep0_buf[4];   /* wIndex low  */
    uint16_t       wlen = (uint16_t)(ep0_buf[6] | (ep0_buf[7] << 8));
    const uint8_t *p    = 0;
    uint32_t       len  = 0;
    uint8_t        err  = 0;

    R8_UEP0_CTRL = RB_UEP_R_TOG | RB_UEP_T_TOG | UEP_R_RES_ACK | UEP_T_RES_NAK;
    setup_req = rq;

    if ((bm & 0x60) == 0x20) {              /* class (HID) */
        if (rq != HIDRQ_SET_IDLE)
            err = 1;                        /* GET/SET_REPORT, GET_IDLE: STALL */
    } else if ((bm & 0x60) != 0x00) {
        err = 1;                            /* vendor: STALL */
    } else if (rq == RQ_GET_DESCRIPTOR) {
        if (vh == 0x01)      { p = dev_desc;    len = sizeof dev_desc;    }
        else if (vh == 0x02) { p = cfg_desc;    len = sizeof cfg_desc;    }
        else if (vh == 0x22) { p = report_desc; len = sizeof report_desc; }
        else if (vh == 0x03) {
            if (vl == 0)      { p = str_lang;     len = sizeof str_lang;     }
            else if (vl == 3) { p = str_serial;   len = sizeof str_serial;   }
            else if (vl <= 2) { p = str_openboot; len = sizeof str_openboot; }
            else              { err = 1; }
        }
        else err = 1;
    } else if (rq == RQ_SET_ADDRESS) {
        pend_addr = vl;
    } else if (rq == RQ_GET_CONFIGURATION) {
        ep0_buf[0] = dev_config;
        len = 1;
    } else if (rq == RQ_SET_CONFIGURATION) {
        dev_config = vl;
    } else if (rq == RQ_GET_INTERFACE) {
        ep0_buf[0] = 0;
        len = 1;
    } else if (rq == RQ_GET_STATUS) {
        ep0_buf[0] = 0;
        ep0_buf[1] = 0;
        len = 2;
    } else if (rq == RQ_CLEAR_FEATURE) {
        if ((bm & 0x1F) == 0x02) {          /* endpoint recipient */
            if (ixl == 0x81)
                R8_UEP1_CTRL = (R8_UEP1_CTRL & ~(RB_UEP_T_TOG | MASK_UEP_T_RES)) | UEP_T_RES_NAK;
            else if (ixl == 0x01)
                R8_UEP1_CTRL = (R8_UEP1_CTRL & ~(RB_UEP_R_TOG | MASK_UEP_R_RES)) | UEP_R_RES_ACK;
            else
                err = 1;
        }                                   /* device recipient: zero-len ack */
    } else if (rq != RQ_SET_FEATURE) {      /* SET_FEATURE: minimal zero-len ack */
        err = 1;
    }

    if (err) {
        R8_UEP0_CTRL = RB_UEP_R_TOG | RB_UEP_T_TOG | UEP_R_RES_STALL | UEP_T_RES_STALL;
        return;
    }
    if (bm & 0x80) {
        /* IN data stage. Every descriptor is under 64 bytes, so one
         * packet always suffices and no continuation logic exists. */
        if (len > wlen)
            len = wlen;
        if (p) {
            uint32_t i;
            for (i = 0; i < len; i++)
                ep0_buf[i] = p[i];
        }
    } else {
        len = 0;                            /* status-IN only */
    }
    R8_UEP0_T_LEN = (uint8_t)len;
    R8_UEP0_CTRL  = RB_UEP_R_TOG | RB_UEP_T_TOG | UEP_R_RES_ACK | UEP_T_RES_ACK;
}

/* Event pump, mirroring the EVT USB_DevTransProcess structure. */
static void usb_process(void)
{
    uint8_t intflag = R8_USB_INT_FG;

    if (intflag & RB_UIF_TRANSFER) {
        /* A SETUP transaction must fall through to ep0_setup() WITHOUT
         * clearing RB_UIF_TRANSFER first. RB_UC_INT_BUSY (set in tr_init)
         * makes the engine auto-NAK while that flag is set, and that is the
         * only thing holding the 8-byte setup packet still in the EP0 DMA
         * buffer: clear the flag early and the next transaction may land on
         * top of the request ep0_setup() is about to parse. The host sees
         * the resulting garbage or STALL as a failed "device descriptor
         * read/64" (EPIPE) during enumeration.
         *
         * A SETUP token reads as MASK_UIS_TOKEN in the token field, which is
         * the same guard CH57x_usbdev.c uses. */
        if ((R8_USB_INT_ST & MASK_UIS_TOKEN) != UIS_TOKEN_SETUP &&
            !(intflag & RB_U_IS_NAK)) {
            switch (R8_USB_INT_ST & (MASK_UIS_TOKEN | MASK_UIS_ENDP)) {
            case UIS_TOKEN_IN | 1:          /* EP1 IN drained by host */
                R8_UEP1_CTRL = (R8_UEP1_CTRL & ~MASK_UEP_T_RES) | UEP_T_RES_NAK;
                tx_busy = 0;
                break;
            case UIS_TOKEN_OUT | 1:         /* EP1 OUT: report arrived */
                /* Only full 64-byte reports are frames (protocol: one
                 * zero-padded frame per report). A short packet would
                 * leave the tail of the previous report in the DMA
                 * buffer — replayable as a stale-but-CRC-valid frame. */
                if ((R8_USB_INT_ST & RB_UIS_TOG_OK) &&
                    R8_USB_RX_LEN == OB_MAX_FRAME) {
                    R8_UEP1_CTRL = (R8_UEP1_CTRL & ~MASK_UEP_R_RES) | UEP_R_RES_NAK;
                    rx_state = 1;
                }
                break;
            case UIS_TOKEN_IN | 0:          /* EP0 IN done */
                if (setup_req == RQ_SET_ADDRESS)
                    R8_USB_DEV_AD = pend_addr;
                R8_UEP0_T_LEN = 0;
                R8_UEP0_CTRL  = UEP_R_RES_ACK | UEP_T_RES_NAK;
                break;
            case UIS_TOKEN_OUT | 0:         /* EP0 status/extra OUT */
                R8_UEP0_CTRL = UEP_R_RES_ACK | UEP_T_RES_NAK;
                break;
            default:
                break;
            }
        }
        if (R8_USB_INT_ST & RB_UIS_SETUP_ACT)
            ep0_setup();
        R8_USB_INT_FG = RB_UIF_TRANSFER;
    } else if (intflag & RB_UIF_BUS_RST) {
        R8_USB_DEV_AD = 0;
        usb_ep_reset();
        R8_USB_INT_FG = RB_UIF_BUS_RST;
    } else if (intflag) {
        R8_USB_INT_FG = intflag;            /* suspend/resume: just clear */
    }
}

static uint8_t hexnib(uint8_t n)
{
    return (uint8_t)(n < 10 ? '0' + n : 'A' - 10 + n);
}

void tr_init(void)
{
    uint8_t  uid[8];
    uint32_t i;

    ob_read_uid(uid);
    str_serial[0] = sizeof str_serial;
    str_serial[1] = 0x03;
    /* MSB-first over the little-endian u64 the HELLO payload carries, so
     * this string equals the host tool's %016X rendering of the UID. */
    for (i = 0; i < 8; i++) {
        str_serial[2 + 4 * i] = hexnib((uint8_t)(uid[7 - i] >> 4));
        str_serial[4 + 4 * i] = hexnib(uid[7 - i] & 0x0Fu);
    }

    /* Init sequence per the EVT USB_IAP, trimmed to EP0 + EP1. */
    R8_USB_CTRL   = 0x00;
    R8_UEP4_1_MOD = RB_UEP1_RX_EN | RB_UEP1_TX_EN;  /* EP1 OUT+IN; EP4 off */
    R16_UEP0_DMA  = (uint16_t)(uintptr_t)ep0_buf;
    R16_UEP1_DMA  = (uint16_t)(uintptr_t)ep1_buf;
    usb_ep_reset();
    R8_USB_DEV_AD = 0x00;
    R8_USB_CTRL   = RB_UC_DEV_PU_EN | RB_UC_INT_BUSY | RB_UC_DMA_EN;
    R8_USB_INT_FG = 0xFF;
    R8_UDEV_CTRL  = RB_UD_PD_DIS | RB_UD_PORT_EN;
    R8_USB_INT_EN = 0;                              /* fully polled */
    OB_USB_PHY_ATTACH();
}

const uint8_t *tr_poll(uint32_t *avail)
{
    if (rx_state == 2) {
        /* Core consumed the previous report: hand EP1 OUT back to the
         * hardware only now, so the buffer stayed valid in between. */
        rx_state = 0;
        R8_UEP1_CTRL = (R8_UEP1_CTRL & ~MASK_UEP_R_RES) | UEP_R_RES_ACK;
    }
    usb_process();
    if (rx_state == 1) {
        rx_state = 2;
        *avail = OB_MAX_FRAME;
        return ep1_buf;                 /* RX half, 4-byte aligned */
    }
    return 0;
}

void tr_send(const uint8_t *frame, uint32_t len)
{
    uint8_t *tx = ep1_buf + 64;
    uint32_t i;

    for (i = 0; i < OB_MAX_FRAME; i++)
        tx[i] = (i < len) ? frame[i] : 0;

    R8_UEP1_T_LEN = OB_MAX_FRAME;
    R8_UEP1_CTRL  = (R8_UEP1_CTRL & ~MASK_UEP_T_RES) | UEP_T_RES_ACK;
    tx_busy = 1;

    /* Bounded ~10 ms wait for the host to poll the IN endpoint, still
     * servicing EP0 (and a possible early EP1 OUT) meanwhile. */
    for (i = 0; i < 10000u / OB_POLL_INTERVAL_US && tx_busy; i++) {
        usb_process();
        ob_delay_us(OB_POLL_INTERVAL_US);
    }
    if (tx_busy) {                      /* host never came: give up */
        R8_UEP1_CTRL = (R8_UEP1_CTRL & ~MASK_UEP_T_RES) | UEP_T_RES_NAK;
        tx_busy = 0;
    }
}

void tr_deinit(void)
{
    R8_USB_CTRL = RB_UC_RESET_SIE;
    OB_USB_PHY_DETACH();
    ob_delay_us(10000);
}
