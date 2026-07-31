/* OBP v0.1 engine. All frame/argument validation lives here — transports
 * only move bytes, ports only touch hardware. */
#include <string.h>

#include "boot_core.h"
#include "boot_decision.h"
#include "crc32.h"
#include "openboot_port.h"

#ifndef OB_TRANSPORT_ID
#error "build must inject OB_TRANSPORT_ID"
#endif

#include "ob_xip.h"

/* --- state ------------------------------------------------------------
 *
 * Three independent concepts, each with exactly one representation:
 *
 *   session   IDLE vs SESSION (protocol section 5). Session-scoped.
 *   stream    how COMMIT may attest on parts without FEAT_CRC_LIVE:
 *             an unbroken sequential run, or poisoned. Session-scoped.
 *   record    what is known about the boot record. POWER-CYCLE-scoped:
 *             HELLO re-opens a session but cannot restore XIP coherence,
 *             so on CH57x (F26) a stale pre-write view can outlive any
 *             number of sessions. Reset by ob_core_init() only.
 *
 * OB_REC_FLASH doubles as "no controller write since reset", which is
 * exactly the bless precondition: every path that dirties flash goes
 * through mutation_begin() or record_begin_write(), and both leave the
 * record state != FLASH. That is why no separate flash_dirty flag exists.
 *
 * Handlers validate arguments, call ONE transition helper, perform the
 * port operation, and build a response — they never touch these fields
 * directly. Ordering rules live in the helpers, next to the state.
 */
#define OB_STREAM_RUN      0u  /* sequential run intact, crc_state usable */
#define OB_STREAM_POISONED 1u  /* flash no longer matches crc_state       */

static struct {
    uint8_t  session;                    /* HELLO seen this power cycle */
    uint8_t  disarmed;                   /* record invalidated this session */
    uint8_t  stream;                     /* OB_STREAM_*                 */
    uint8_t  bitmap[OB_BITMAP_BYTES];    /* blocks erased this session  */
    uint32_t expected_next;              /* next sequential WRITE address */
    uint32_t last_addr;                  /* last chunk folded into the stream */
    uint32_t last_len;
    uint32_t crc_state;                  /* running stream CRC (pre-final) */
    uint8_t  last_data[OB_MAX_WRITE_DATA]; /* bytes of the last folded chunk
                                            * (retry must be byte-exact)   */
} s;

#define OB_REC_FLASH   0u  /* record untouched since reset: flash is truth  */
#define OB_REC_INVALID 1u  /* invalidated, not re-committed: reject BOOT    */
#define OB_REC_FRESH   2u  /* committed this power cycle: RAM says valid    */

static uint8_t rec_state;                /* power-cycle-scoped, see above */
static uint32_t committed_img_len;        /* valid only in OB_REC_FRESH */
static uint32_t committed_img_crc;        /* exact replay key */

/* WRITE classification against the sequential run. */
#define WR_FOLD   0u   /* next chunk of the run: fold it on success */
#define WR_RETRY  1u   /* byte-exact re-send of the previous chunk  */
#define WR_POISON 2u   /* anything else: the run is over            */

static uint32_t get32(const uint8_t *p)
{
    return (uint32_t)p[0] | ((uint32_t)p[1] << 8) |
           ((uint32_t)p[2] << 16) | ((uint32_t)p[3] << 24);
}

static void put32(uint8_t *p, uint32_t v)
{
    p[0] = (uint8_t)v;
    p[1] = (uint8_t)(v >> 8);
    p[2] = (uint8_t)(v >> 16);
    p[3] = (uint8_t)(v >> 24);
}

/* Seal a response whose payload (n bytes) is already at resp+4. */
static uint32_t finish(uint8_t *resp, uint8_t cmd, uint8_t seq, uint32_t n)
{
    resp[0] = cmd;
    resp[1] = seq;
    resp[2] = (uint8_t)n;
    resp[3] = 0;
    put32(resp + OB_FRAME_HDR_LEN + n, ob_crc32(resp, OB_FRAME_HDR_LEN + n));
    return OB_FRAME_OVERHEAD + n;
}

static uint32_t err_resp(uint8_t *resp, uint8_t cmd, uint8_t seq,
                         uint8_t status, uint8_t detail)
{
    resp[4] = status;
    resp[5] = detail;
    return finish(resp, cmd | OB_CMD_RESP_BIT, seq, 2);
}

static uint32_t ok_resp(uint8_t *resp, uint8_t cmd, uint8_t seq)
{
    resp[4] = OB_OK;
    return finish(resp, cmd | OB_CMD_RESP_BIT, seq, 1);
}

/* Single shared range gate: [addr, addr+len) inside the app region, len
 * nonzero, overflow-safe. The bootloader region is unreachable by design. */
static int range_ok(uint32_t addr, uint32_t len)
{
    uint32_t end = ob_app_end();

    return addr >= OB_FLASH_APP_START && addr < end &&
           len != 0 && len <= end - addr;
}

/* --- state transitions ------------------------------------------------ */

/* (Re-)open a session: bitmap, stream run and disarm state all reset. The
 * record state deliberately survives — it is power-cycle-scoped. */
static void session_open(void)
{
    memset(&s, 0, sizeof s);
    s.session = 1;
    s.stream = OB_STREAM_RUN;
    s.expected_next = OB_FLASH_APP_START;
    s.crc_state = ob_crc32_init();
}

/* Disarm-before-mutation: the first ERASE/WRITE of a session kills the boot
 * record so an interrupted update can never boot a stale image. Returns ROM
 * code; nonzero means app flash was NOT touched. */
static uint32_t mutation_begin(void)
{
    uint32_t r;

    if (s.disarmed)
        return 0;
    /* Distrust the record from the moment mutation is attempted: a failed
     * invalidate leaves the page in an unknown state, and after a good one
     * F26 can keep serving the stale pre-erase record via XIP. */
    rec_state = OB_REC_INVALID;
    committed_img_len = 0;
    committed_img_crc = 0;
    r = ob_record_invalidate();
    if (r == 0)
        s.disarmed = 1;
    return r;
}

/* An erase over already-streamed bytes leaves crc_state describing content
 * that no longer exists. MUST be called before the fallible erase loop: an
 * early block can succeed (destroying streamed bytes) while a later one
 * fails, and that early return must not leave the stream attestable. */
static void stream_note_erase(uint32_t addr)
{
    if (addr < s.expected_next)
        s.stream = OB_STREAM_POISONED;
}

/* Classify a WRITE against the run and poison immediately if it breaks it.
 * MUST be called before the fallible flash ops for the same reason as
 * stream_note_erase(). A same-range write with DIFFERENT bytes (further
 * 1->0 programming the controller accepts) is NOT a retry. */
static uint32_t stream_classify_write(uint32_t addr, uint32_t len,
                                      const uint8_t *data)
{
    if (addr == s.expected_next)
        return WR_FOLD;
    if (addr == s.last_addr && len == s.last_len &&
        addr + len == s.expected_next &&
        memcmp(data, s.last_data, len) == 0)
        return WR_RETRY;
    s.stream = OB_STREAM_POISONED;
    return WR_POISON;
}

/* Fold a verified chunk into the run (WR_FOLD, after the write succeeded). */
static void stream_fold(uint32_t addr, uint32_t len, const uint8_t *data)
{
    s.crc_state = ob_crc32_update(s.crc_state, data, len);
    s.expected_next += len;
    s.last_addr = addr;
    s.last_len = len;
    memcpy(s.last_data, data, len);
}

/* Erased-block bitmap: WRITE may only touch blocks erased this session. */
static void bitmap_mark(uint32_t addr)
{
    uint32_t blk = (addr - OB_FLASH_APP_START) / OB_FLASH_ERASE_BLOCK;

    s.bitmap[blk >> 3] |= (uint8_t)(1u << (blk & 7u));
}

static int bitmap_covers(uint32_t addr, uint32_t len)
{
    uint32_t first = (addr - OB_FLASH_APP_START) / OB_FLASH_ERASE_BLOCK;
    uint32_t last = (addr + len - 1 - OB_FLASH_APP_START) / OB_FLASH_ERASE_BLOCK;
    uint32_t b;

    for (b = first; b <= last; b++)
        if (!(s.bitmap[b >> 3] & (1u << (b & 7u))))
            return 0;
    return 1;
}

/* May COMMIT read the image straight out of flash? Either the part says XIP
 * is always coherent, or nothing has been written since reset (the bless
 * precondition — see the record-state note above). */
static int attest_via_xip(void)
{
    return (OB_FEATURES & OB_FEAT_CRC_LIVE) || rec_state == OB_REC_FLASH;
}

/* Does the sequential run cover exactly [app_start, app_start + img_len)? */
static int stream_covers(uint32_t img_len)
{
    return s.stream == OB_STREAM_RUN &&
           s.expected_next == OB_FLASH_APP_START + img_len;
}

/* The run's CRC over everything folded so far (valid iff stream_covers). */
static uint32_t stream_crc(void)
{
    return ob_crc32_final(s.crc_state);
}

static void record_begin_write(void)
{
    rec_state = OB_REC_INVALID;          /* unknown until the write succeeds */
    committed_img_len = 0;
    committed_img_crc = 0;
}

static void record_note_committed(uint32_t img_len, uint32_t img_crc)
{
    rec_state = OB_REC_FRESH;
    committed_img_len = img_len;
    committed_img_crc = img_crc;
    s.disarmed = 0;                      /* a later mutation must re-disarm */
}

/* The failed write may still have landed a complete, CRC-valid record, so
 * stop claiming the record is invalidated: the next mutation must
 * re-invalidate it or a power cut mid-rewrite could boot a torn image. */
static void record_note_write_failed(void)
{
    s.disarmed = 0;
}

/* BOOT's record gate. RAM truth beats the flash record wherever the two can
 * disagree under F26: after an invalidate the flash may still read as the
 * old (valid) record, and after a fresh COMMIT it may still read as erased.
 * Only the untouched-since-reset state consults flash — and then applies
 * the FULL boot-decision validation, so an explicit BOOT can never launch
 * an app the reset path would refuse. */
static int boot_record_trusted(void)
{
    if (rec_state == OB_REC_FRESH)
        return 1;
    return rec_state == OB_REC_FLASH && ob_boot_app_valid();
}

void ob_core_init(void)
{
    memset(&s, 0, sizeof s);
    rec_state = OB_REC_FLASH;
    committed_img_len = 0;
    committed_img_crc = 0;
}

int ob_core_session_active(void)
{
    return s.session;
}

static uint32_t do_hello(const uint8_t *pl, uint8_t n, uint8_t seq, uint8_t *resp)
{
    uint8_t *r = resp + OB_FRAME_HDR_LEN;

    if (n != OB_HELLO_REQ_LEN)
        return err_resp(resp, OB_CMD_HELLO, seq, OB_E_LEN, 0);
    if (get32(pl) != OB_HELLO_MAGIC)
        return err_resp(resp, OB_CMD_HELLO, seq, OB_E_ARG, 0);
    /* Pre-1.0 (major 0) nothing is frozen: require an exact minor match
     * too. From 1.0 on, only the major gates (minors are additive). */
    if (pl[4] != OB_PROTO_MAJOR ||
        (OB_PROTO_MAJOR == 0 && pl[5] != OB_PROTO_MINOR))
        return err_resp(resp, OB_CMD_HELLO, seq, OB_E_PROTO, 0);

    session_open();

    r[0] = OB_OK;
    r[1] = OB_PROTO_MAJOR;
    r[2] = OB_PROTO_MINOR;
    r[3] = ob_chip_rev();
    r[4] = (uint8_t)OB_BL_VERSION;
    r[5] = (uint8_t)(OB_BL_VERSION >> 8);
    r[6] = OB_CHIP_FAMILY;
    r[7] = OB_TRANSPORT_ID;
    put32(r + 8, OB_FLASH_APP_START);
    put32(r + 12, ob_app_end());
    put32(r + 16, OB_FLASH_ERASE_BLOCK);
    r[20] = (uint8_t)OB_FLASH_WRITE_PAGE;
    r[21] = (uint8_t)(OB_FLASH_WRITE_PAGE >> 8);
    r[22] = 4;                       /* write alignment */
    r[23] = OB_MAX_WRITE_DATA;
    put32(r + 24, OB_FEATURES);
    ob_read_uid(r + 28);
    return finish(resp, OB_CMD_HELLO | OB_CMD_RESP_BIT, seq, OB_HELLO_RESP_LEN);
}

static uint32_t do_erase(const uint8_t *pl, uint8_t n, uint8_t seq, uint8_t *resp)
{
    uint32_t addr, len, r, a;

    if (n != 8)
        return err_resp(resp, OB_CMD_ERASE, seq, OB_E_LEN, 0);
    addr = get32(pl);
    len = get32(pl + 4);
    if ((addr % OB_FLASH_ERASE_BLOCK) || (len % OB_FLASH_ERASE_BLOCK))
        return err_resp(resp, OB_CMD_ERASE, seq, OB_E_ADDR, OB_DET_ADDR_ALIGN);
    if (!range_ok(addr, len))
        return err_resp(resp, OB_CMD_ERASE, seq, OB_E_ADDR, OB_DET_ADDR_RANGE);
    stream_note_erase(addr);
    r = mutation_begin();
    if (r)
        return err_resp(resp, OB_CMD_ERASE, seq, OB_E_FLASH, (uint8_t)r);
    for (a = addr; a < addr + len; a += OB_FLASH_ERASE_BLOCK) {
        r = ob_flash_erase(a, OB_FLASH_ERASE_BLOCK);
        if (r)
            return err_resp(resp, OB_CMD_ERASE, seq, OB_E_FLASH, (uint8_t)r);
        bitmap_mark(a);
    }
    return ok_resp(resp, OB_CMD_ERASE, seq);
}

static uint32_t do_write(const uint8_t *pl, uint8_t n, uint8_t seq, uint8_t *resp)
{
    uint32_t addr, dlen, r, class;
    const uint8_t *data = pl + 4;    /* frame buffer is 4-aligned; offset 8 */

    if (n < 8 || (uint32_t)(n - 4) > OB_MAX_WRITE_DATA || ((n - 4) % 4))
        return err_resp(resp, OB_CMD_WRITE, seq, OB_E_LEN, 0);
    addr = get32(pl);
    dlen = (uint32_t)(n - 4);
    if (addr % 4)
        return err_resp(resp, OB_CMD_WRITE, seq, OB_E_ADDR, OB_DET_ADDR_ALIGN);
    if (!range_ok(addr, dlen))
        return err_resp(resp, OB_CMD_WRITE, seq, OB_E_ADDR, OB_DET_ADDR_RANGE);
    if (!bitmap_covers(addr, dlen))
        return err_resp(resp, OB_CMD_WRITE, seq, OB_E_NOT_ERASED, 0);

    class = stream_classify_write(addr, dlen, data);

    r = mutation_begin();
    if (r)
        return err_resp(resp, OB_CMD_WRITE, seq, OB_E_FLASH, (uint8_t)r);
    r = ob_flash_write(addr, data, dlen);
    if (r == 0)
        r = ob_flash_verify(addr, data, dlen);
    if (r)
        return err_resp(resp, OB_CMD_WRITE, seq, OB_E_FLASH, (uint8_t)r);

    if (class == WR_FOLD)
        stream_fold(addr, dlen, data);
    return ok_resp(resp, OB_CMD_WRITE, seq);
}

static uint32_t do_crc(const uint8_t *pl, uint8_t n, uint8_t seq, uint8_t *resp)
{
    uint32_t addr, len;

    if (n != 8)
        return err_resp(resp, OB_CMD_CRC, seq, OB_E_LEN, 0);
    addr = get32(pl);
    len = get32(pl + 4);
    if ((addr % 4) || (len % 4))
        return err_resp(resp, OB_CMD_CRC, seq, OB_E_ADDR, OB_DET_ADDR_ALIGN);
    if (!range_ok(addr, len))
        return err_resp(resp, OB_CMD_CRC, seq, OB_E_ADDR, OB_DET_ADDR_RANGE);
    resp[4] = OB_OK;
    put32(resp + 5, ob_xip_crc32(addr, len));
    return finish(resp, OB_CMD_CRC | OB_CMD_RESP_BIT, seq, 5);
}

static uint32_t do_commit(const uint8_t *pl, uint8_t n, uint8_t seq, uint8_t *resp)
{
    uint32_t img_len, img_crc, c, r;
    ob_boot_record_t rec;

    if (n != 8)
        return err_resp(resp, OB_CMD_COMMIT, seq, OB_E_LEN, 0);
    img_len = get32(pl);
    img_crc = get32(pl + 4);
    if (img_len == 0 || (img_len % 4))
        return err_resp(resp, OB_CMD_COMMIT, seq, OB_E_LEN, 0);
    if (img_len > ob_app_end() - OB_FLASH_APP_START)
        return err_resp(resp, OB_CMD_COMMIT, seq, OB_E_ADDR, OB_DET_ADDR_RANGE);

    /* A lost success response is safe to retry. The record write itself
     * makes CH57x XIP unsuitable for re-attestation, so remember the exact
     * tuple that reached flash and acknowledge only that tuple without
     * touching the controller again. HELLO deliberately preserves this
     * power-cycle state; reset and every mutation clear it. */
    if (rec_state == OB_REC_FRESH && committed_img_len == img_len &&
        committed_img_crc == img_crc)
        return ok_resp(resp, OB_CMD_COMMIT, seq);

    if (attest_via_xip()) {
        c = ob_xip_crc32(OB_FLASH_APP_START, img_len);
    } else {
        /* F26: XIP over freshly-written flash may be stale — only a fully
         * sequential stream from app_start can be attested. */
        if (!stream_covers(img_len))
            return err_resp(resp, OB_CMD_COMMIT, seq, OB_E_VERIFY,
                            OB_DET_VERIFY_NONSEQ);
        c = stream_crc();
    }
    if (c != img_crc)
        return err_resp(resp, OB_CMD_COMMIT, seq, OB_E_VERIFY,
                        OB_DET_VERIFY_MISMATCH);

    rec.magic = OB_RECORD_MAGIC;
    rec.img_len = img_len;
    rec.img_crc32 = img_crc;
    rec.rec_crc32 = ob_crc32(&rec, 12);
    record_begin_write();
    r = ob_record_write(&rec);
    if (r) {
        record_note_write_failed();
        return err_resp(resp, OB_CMD_COMMIT, seq, OB_E_FLASH, (uint8_t)r);
    }
    record_note_committed(img_len, img_crc);
    return ok_resp(resp, OB_CMD_COMMIT, seq);
}

static ob_action_t do_boot(const uint8_t *pl, uint8_t n, uint8_t seq,
                           uint8_t *resp, uint32_t *resp_len)
{
    if (n != 1) {
        *resp_len = err_resp(resp, OB_CMD_BOOT, seq, OB_E_LEN, 0);
        return OB_ACT_NONE;
    }
    if (pl[0] > OB_BOOT_STAY) {
        *resp_len = err_resp(resp, OB_CMD_BOOT, seq, OB_E_ARG, 0);
        return OB_ACT_NONE;
    }
    if (pl[0] == OB_BOOT_APP) {
        if (!boot_record_trusted()) {
            *resp_len = err_resp(resp, OB_CMD_BOOT, seq, OB_E_VERIFY,
                                 OB_DET_VERIFY_NORECORD);
            return OB_ACT_NONE;
        }
        *resp_len = ok_resp(resp, OB_CMD_BOOT, seq);
        /* Reset-to-launch, always and on every family: no boot-request
         * magic is written, so the post-reset boot decision — the single
         * launch authority — validates with coherent XIP and starts the
         * app. On CH57x (F26) the reset is also what makes freshly
         * written code safe to execute. */
        return OB_ACT_RESET;
    }
    /* Stay-in-bootloader must survive the reset even with a valid app: the
     * boot decision consumes this magic on the way back up (RAM is retained
     * across a soft reset). */
    *(volatile uint32_t *)(uintptr_t)(OB_BOOTREQ_ADDR) = OB_BOOTREQ_MAGIC;
    *resp_len = ok_resp(resp, OB_CMD_BOOT, seq);
    return OB_ACT_RESET;
}

ob_action_t ob_core_handle_frame(const uint8_t *buf, uint32_t avail,
                                 uint8_t *resp, uint32_t *resp_len)
{
    uint8_t cmd, seq, n;
    const uint8_t *pl = buf + OB_FRAME_HDR_LEN;

    *resp_len = 0;
    if (avail < OB_FRAME_OVERHEAD)
        return OB_ACT_NONE;
    cmd = buf[0];
    seq = buf[1];
    n = buf[2];
    if (n > OB_MAX_PAYLOAD)                       /* undecodable: no response */
        return OB_ACT_NONE;
    if (avail < (uint32_t)OB_FRAME_OVERHEAD + n)
        return OB_ACT_NONE;
    if (ob_crc32(buf, (uint32_t)OB_FRAME_HDR_LEN + n) !=
        get32(buf + OB_FRAME_HDR_LEN + n)) {
        resp[4] = OB_E_CRC;
        resp[5] = 0;
        *resp_len = finish(resp, OB_CMD_FRAME_ERR, seq, 2);
        return OB_ACT_NONE;
    }
    if (buf[3] != 0) {
        *resp_len = err_resp(resp, cmd, seq, OB_E_ARG, 0);
        return OB_ACT_NONE;
    }

    switch (cmd) {
    case OB_CMD_HELLO:
        *resp_len = do_hello(pl, n, seq, resp);
        return OB_ACT_NONE;
    case OB_CMD_BOOT:
    case OB_CMD_ERASE:
    case OB_CMD_WRITE:
    case OB_CMD_CRC:
    case OB_CMD_COMMIT:
        /* IDLE accepts only HELLO (protocol section 5) — BOOT included:
         * a pre-handshake frame must not be able to reset the device or
         * launch the app. */
        if (!s.session) {
            *resp_len = err_resp(resp, cmd, seq, OB_E_STATE, 0);
            return OB_ACT_NONE;
        }
        if (cmd == OB_CMD_BOOT)
            return do_boot(pl, n, seq, resp, resp_len);
        if (cmd == OB_CMD_ERASE)
            *resp_len = do_erase(pl, n, seq, resp);
        else if (cmd == OB_CMD_WRITE)
            *resp_len = do_write(pl, n, seq, resp);
        else if (cmd == OB_CMD_CRC)
            *resp_len = do_crc(pl, n, seq, resp);
        else
            *resp_len = do_commit(pl, n, seq, resp);
        return OB_ACT_NONE;
    default:                                      /* incl. READ: off in v1 */
        *resp_len = err_resp(resp, cmd, seq, OB_E_CMD, 0);
        return OB_ACT_NONE;
    }
}
