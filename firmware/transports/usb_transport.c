/*
 * OpenBoot USB transport: full-speed HID device, fully polled (USB
 * interrupts are never enabled; RB_UC_INT_BUSY auto-NAKs while an event
 * is pending, so the 20 us poll cadence loses nothing).
 *
 * Identity: VID 0x1209 PID 0x0001 (pid.codes test range) by default,
 * overridable per board with OB_USB_VID / OB_USB_PID so a product can
 * enumerate under its own identity instead of a bootloader-specific one.
 * One vendor usage-page HID interface, EP1 interrupt IN+OUT, 64-byte
 * reports, no report IDs. One OBP frame per report, zero-padded; the core
 * does ALL frame validation. iManufacturer and iProduct share one
 * "OpenBoot" string to save flash (a per-family product name would cost
 * ~40 bytes of rodata for no protocol value — HELLO reports the chip
 * family), and it doubles as the human-visible signal that a device
 * sharing its application's VID:PID is currently in the bootloader.
 *
 * A board that shares its application's VID:PID makes VID:PID alone
 * ambiguous. The report descriptor below declares vendor usage page
 * 0xFF00 usage 0x01, which is what a host adds to VID:PID to tell the two
 * apart; a board doing this must keep its application off that usage.
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

/* pid.codes test allocation; boards override via the Makefile knobs. */
#ifndef OB_USB_VID
#define OB_USB_VID 0x1209u
#endif
#ifndef OB_USB_PID
#define OB_USB_PID 0x0001u
#endif
_Static_assert(OB_USB_VID <= 0xFFFFu, "OB_USB_VID must fit in 16 bits");
_Static_assert(OB_USB_PID <= 0xFFFFu, "OB_USB_PID must fit in 16 bits");

/* bcdDevice is binary-coded decimal; OB_BL_VERSION is binary, major in the
 * high byte and minor in the low. Each byte is converted on its own, so
 * v0.13 (0x000D) is announced as 0x0013 and reads "0.13" to a host. The
 * descriptor used to carry the binary word, which is not BCD at all for a
 * minor of 10 to 15 (Windows printed REV_000; for v0.12) and would have
 * read as the wrong version from 16 on (0x0010 is BCD for 0.10). */
#define OB_BL_MAJOR  ((OB_BL_VERSION >> 8) & 0xFFu)
#define OB_BL_MINOR  (OB_BL_VERSION & 0xFFu)
#define OB_BCD8(x)   ((uint8_t)((((x) / 10u) << 4) | ((x) % 10u)))
_Static_assert(OB_BL_MAJOR < 100u && OB_BL_MINOR < 100u,
               "OB_BL_VERSION: each byte must be under 100 to fit bcdDevice");

static const uint8_t dev_desc[18] = {
    18,   0x01,             /* bLength, DEVICE                       */
    0x10, 0x01,             /* bcdUSB 1.10                           */
    0x00, 0x00, 0x00,       /* class/subclass/protocol per interface */
    64,                     /* bMaxPacketSize0                       */
    (uint8_t)OB_USB_VID, (uint8_t)(OB_USB_VID >> 8),
    (uint8_t)OB_USB_PID, (uint8_t)(OB_USB_PID >> 8),
    OB_BCD8(OB_BL_MINOR),
    OB_BCD8(OB_BL_MAJOR),   /* bcdDevice = bootloader version, in BCD */
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
    uint8_t        bm, rq, vl, vh, ixl;
    uint16_t       wlen;
    const uint8_t *p    = 0;
    uint32_t       len  = 0;
    uint8_t        err  = 0;

    /* Closed in both directions before the request is even read, and until
     * its reply has been copied into the buffer it arrived in. With a lone
     * SETUP the engine NAKs anyway until RB_UIF_TRANSFER is cleared; after
     * a transfer was retired in front of it (usb_process) that is not
     * documented to hold. Every path below ends by writing the whole
     * register. This cannot keep a second SETUP out: the engine takes one
     * whatever the endpoint is set to answer. */
    R8_UEP0_CTRL = RB_UEP_R_TOG | RB_UEP_T_TOG | UEP_R_RES_NAK | UEP_T_RES_NAK;

    bm   = ep0_buf[0];
    rq   = ep0_buf[1];
    vl   = ep0_buf[2];                      /* wValue low  */
    vh   = ep0_buf[3];                      /* wValue high */
    ixl  = ep0_buf[4];                      /* wIndex low  */
    wlen = (uint16_t)(ep0_buf[6] | (ep0_buf[7] << 8));
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
        /* A host starts both of EP1's data sequences from DATA0 again when
         * it sets a configuration (USB 2.0, 9.1.1.5), reset or no reset in
         * front of it. A toggle left at DATA1 would have the next report
         * out ACKed and dropped as a repeat, or the next one in dropped by
         * the host. EP1's answers stay as they are: see CLEAR_FEATURE
         * below. */
        R8_UEP1_CTRL = R8_UEP1_CTRL & ~(RB_UEP_R_TOG | RB_UEP_T_TOG);
    } else if (rq == RQ_GET_INTERFACE) {
        ep0_buf[0] = 0;
        len = 1;
    } else if (rq == RQ_GET_STATUS) {
        ep0_buf[0] = 0;
        ep0_buf[1] = 0;
        len = 2;
    } else if (rq == RQ_CLEAR_FEATURE) {
        if ((bm & 0x1F) == 0x02) {          /* endpoint recipient */
            /* EP1 is never halted, so there is no halt to clear: all the
             * request has to do here is restart the data sequence. What
             * EP1 answers is not this request's to decide. Apart from the
             * hold across a SETUP in usb_process() and the shutdown in
             * tr_deinit(), OUT is at NAK only from a report's arrival until
             * the tr_poll() after the one that handed it to the core, which
             * reads it in place; opening it here would let the next one
             * overwrite it. IN is at ACK only while a reply is armed, and
             * closing it here would strand that reply until tr_send() gave
             * up on it. tr_poll() and tr_send() own both. A host sends this
             * after transfer errors on a pipe, not only after a STALL, so
             * it does arrive. */
            if (ixl == 0x81)
                R8_UEP1_CTRL = R8_UEP1_CTRL & ~RB_UEP_T_TOG;
            else if (ixl == 0x01)
                R8_UEP1_CTRL = R8_UEP1_CTRL & ~RB_UEP_R_TOG;
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
        /* One sample of the status decides everything below.
         *
         * The token field reads 11b (the SDK names that value
         * UIS_TOKEN_SETUP) when it is IDLE. A SETUP is reported by
         * RB_UIS_SETUP_ACT alone and does not touch the token, endpoint,
         * toggle or length fields (datasheet, R8_USB_INT_ST). So this poll
         * can find three things: a transfer (token not idle), a SETUP
         * (SETUP_ACT with the token idle), or BOTH, when a SETUP was taken
         * before the transfer in front of it had been serviced.
         *
         * For both together the datasheet gives the order, and it is two
         * clears: handle the transfer, clear RB_UIF_TRANSFER to retire it,
         * handle the SETUP, clear again. This code used to handle both and
         * clear once. What the registers then hold is not documented. If
         * the SETUP stays flagged, the next poll runs ep0_setup() a second
         * time, over a buffer that by then holds the reply to the first:
         * the serial string's first bytes (22 03) parse as a class request
         * that is not SET_IDLE and get a STALL; the report descriptor's
         * (06 00) parse as GET_STATUS and the other strings' (12 03) as
         * SET_FEATURE, each answered with an empty packet in place of the
         * descriptor. That would account for a host behind one make of hub
         * getting no serial number and no report descriptor while a root
         * port worked, which is what prompted this; it has not been
         * captured on the wire.
         *
         * A lone SETUP still reaches ep0_setup() with RB_UIF_TRANSFER set:
         * RB_UC_INT_BUSY (tr_init) makes the engine NAK while the flag is
         * set, which keeps the next transaction off the request being
         * parsed. Whether that still holds after the first of two clears is
         * not documented either, so EP0 is closed by hand before it and
         * ep0_setup() opens it again with its last write.
         *
         * EP1 is held at NAK across the same span, for the second clear's
         * sake: a transfer on EP1 that completed while ep0_setup() ran
         * would be retired by that clear without having been seen. Its
         * answers are put back afterwards as they stood, with whatever
         * toggles the request left (a CLEAR_FEATURE on EP1 resets one).
         * Both writes to EP1 are made while it cannot complete a transfer,
         * the first under the pending flag and the second under the hold,
         * so neither can write back a toggle the engine has just moved.
         *
         * A SETUP that arrives after the sample below and before the first
         * clear is found by the re-read and handled like any other:
         * ep0_setup() closes EP0 before it reads the request. EP1 is not
         * held for that one, because nothing then guarantees it is quiet
         * while its register is rewritten. That leaves the late SETUP
         * relying on timing: ep0_setup() has to finish before a 64-byte
         * report that follows the SETUP can complete, 48.7 us at the least,
         * and the path is some 340 instructions at worst, run from flash at
         * a speed that has not been measured. Holding EP1 on every transfer
         * would cover it without the assumption. That was written and ran
         * on hardware with no fault traced to it, and was left out to keep
         * this loop the code that has been tested. */
        uint8_t st       = R8_USB_INT_ST;
        uint8_t setup    = st & RB_UIS_SETUP_ACT;
        uint8_t transfer = (st & MASK_UIS_TOKEN) != UIS_TOKEN_SETUP;
        uint8_t ep1_res  = 0;
        uint8_t ep1_held = 0;

        if (transfer && !(intflag & RB_U_IS_NAK)) {
            switch (st & (MASK_UIS_TOKEN | MASK_UIS_ENDP)) {
            case UIS_TOKEN_IN | 1:          /* EP1 IN drained by host */
                R8_UEP1_CTRL = (R8_UEP1_CTRL & ~MASK_UEP_T_RES) | UEP_T_RES_NAK;
                tx_busy = 0;
                break;
            case UIS_TOKEN_OUT | 1:         /* EP1 OUT: report arrived */
                /* Only full 64-byte reports are frames (protocol: one
                 * zero-padded frame per report). A short packet would
                 * leave the tail of the previous report in the DMA
                 * buffer — replayable as a stale-but-CRC-valid frame. */
                if ((st & RB_UIS_TOG_OK) &&
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
        if (transfer) {
            if (setup) {
                R8_UEP0_CTRL = UEP_R_RES_NAK | UEP_T_RES_NAK;
                ep1_res = R8_UEP1_CTRL & (MASK_UEP_R_RES | MASK_UEP_T_RES);
                R8_UEP1_CTRL = (R8_UEP1_CTRL & ~(MASK_UEP_R_RES | MASK_UEP_T_RES))
                               | UEP_R_RES_NAK | UEP_T_RES_NAK;
                ep1_held = 1;
            }
            R8_USB_INT_FG = RB_UIF_TRANSFER;        /* retires the transfer */
            /* One that arrived while the transfer was being handled is not
             * in st. Whether the clear above left it flagged is not
             * documented, so look again, as the SDK's handlers do. */
            setup |= R8_USB_INT_ST & RB_UIS_SETUP_ACT;
        }
        if (setup) {
            ep0_setup();
            R8_USB_INT_FG = RB_UIF_TRANSFER;        /* retires the SETUP */
            if (ep1_held)
                R8_UEP1_CTRL = (R8_UEP1_CTRL & ~(MASK_UEP_R_RES | MASK_UEP_T_RES))
                               | ep1_res;
        } else if (!transfer) {
            R8_USB_INT_FG = RB_UIF_TRANSFER;        /* neither: just clear */
        }
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

int tr_rx_busy(void)
{
    /* The SIE DMA-fills a whole 64-byte report without CPU help; frame
     * arrival never depends on the main loop's pace. */
    return 0;
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
    /* Same degenerate-divisor guard as the UART path: zero polls here would
     * skip the drain wait entirely rather than bound it. */
    _Static_assert(10000u / OB_POLL_INTERVAL_US > 0,
                   "OB_POLL_INTERVAL_US too large: the USB drain wait rounds to zero polls");
    for (i = 0; i < 10000u / OB_POLL_INTERVAL_US && tx_busy; i++) {
        usb_process();
        ob_delay_us(OB_POLL_INTERVAL_US);
    }
    if (tx_busy) {                      /* host never came: give up */
        R8_UEP1_CTRL = (R8_UEP1_CTRL & ~MASK_UEP_T_RES) | UEP_T_RES_NAK;
        tx_busy = 0;
    }
}

/* How long the device stays attached after the last reply, before the
 * pull-up goes. A workaround for what looks like a race between the detach
 * and the host getting that reply. tr_send() returns when the IN report has
 * been ACKed (or its wait has run out), and behind a high-speed hub the ACK
 * comes from the hub's transaction translator: the host collects the report
 * from the hub in a later microframe. With the detach following at once,
 * the host tool behind a Genesys Logic hub (05E3:0610) saw the device vanish
 * with its last answer unread, the one to BOOT, every time, and reported an
 * update that had succeeded as failed; a root port and another hub
 * delivered it. What exactly loses the race (the hub's handling of the
 * disconnect, or the host cancelling the read) has not been captured.
 *
 * Without this the margin is an accident of how fast the poll loop runs.
 * Where ob_delay_us() executes from flash every poll takes longer, the
 * detach comes later after the ACK, and the same hub got its answer each
 * time it was tried; with the delay loop in RAM it lost it every time.
 * Forty microframes is ample for a complete-split. */
#define USB_DETACH_SETTLE_US 5000u

void tr_deinit(void)
{
    /* Nothing is pumped in this window, so nothing can re-open EP1 OUT (a
     * bus reset would) and from here on no report is ACKed on the wire only
     * to be dropped. One that was ACKed after the last poll and before this
     * write goes down with the detach, as it always did; the host tool
     * sends nothing after BOOT. The hub and the host need nothing more
     * from this side to finish; a SETUP that arrives now goes unanswered
     * until the detach ends it. */
    R8_UEP1_CTRL = (R8_UEP1_CTRL & ~MASK_UEP_R_RES) | UEP_R_RES_NAK;
    ob_delay_us(USB_DETACH_SETTLE_US);

    R8_USB_CTRL = RB_UC_RESET_SIE;
    OB_USB_PHY_DETACH();
    ob_delay_us(10000);
}
