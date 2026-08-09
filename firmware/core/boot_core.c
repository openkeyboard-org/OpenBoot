/* OBP v0.2 engine. All frame/argument validation lives here — transports
 * only move bytes, ports only touch hardware. */
#include <string.h>

#include "boot_core.h"
#include "boot_decision.h"
#include "crc32.h"
#include "openboot_port.h"

/* --- which slot this power cycle writes --------------------------------
 *
 * Mutations target the INACTIVE slot, so the image the device is currently
 * able to boot is never the one being overwritten. That is the whole point
 * of A/B: see docs/AB-UPDATE.md.
 *
 * Both are POWER-CYCLE-scoped and derived from flash exactly once, in
 * ob_core_init(), before anything can have been written. They are
 * deliberately NOT recomputed per session, for two reasons.
 *
 * The target must not move under a host that is mid-update. Re-sending HELLO
 * is a legitimate thing to do after a transport hiccup, and it must answer
 * with the slot the half-written image is actually going into — a session
 * reset that also moved the target would leave the erase bitmap and the
 * bytes already written describing different slots.
 *
 * And re-deriving means re-reading records this power cycle has erased,
 * which is exactly the read CH57x (F26) makes unreliable. Nothing here has
 * to reason about whether that view can be trusted, because nothing here
 * takes it. Only boot_record_trusted() consults flash after a mutation, and
 * it is careful about which slot it asks.
 *
 * COMMIT is the only thing that moves them, because COMMIT is the only thing
 * that changes which slot is bootable. */
static uint32_t active_slot;   /* bootable slot, or OB_SLOT_NONE */
static uint32_t write_slot;    /* the slot ERASE, WRITE and COMMIT address */

static uint32_t other_slot(uint32_t slot)
{
    return slot ^ 1u;
}

/* Invalidate a slot's record by erasing the block that holds it. That block
 * is inside the slot being mutated, so the other slot's record - and with it
 * the device's ability to boot - is never at risk. */
static uint32_t record_invalidate(uint32_t slot)
{
    /* The record owns its block outright, so this erases no image bytes. */
    return ob_flash_erase(ob_slot_record_addr(slot), OB_FLASH_ERASE_BLOCK);
}

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

/* Highest valid record generation observed at reset or committed since. Keep
 * it in RAM because CH57x XIP may not reveal a record written this power cycle;
 * deriving it again could reuse a generation and leave the two slots tied. */
static uint32_t highest_gen;

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

/* Two range gates, because reading and changing are not the same risk.
 *
 * READS (CRC) are bounded by the app region, as they always were. A CRC
 * changes nothing, and a host has to be able to read the slot it is not
 * writing: `openboot verify` runs in a fresh session, where the image it
 * wants to check is the ACTIVE one and the write target is already the
 * other slot. Bounding reads to the write slot would leave a committed
 * image with no way to check it. */
static int read_range_ok(uint32_t addr, uint32_t len)
{
    uint32_t end = ob_app_end();

    return addr >= OB_FLASH_APP_START && addr < end &&
           len != 0 && len <= end - addr;
}

/* MUTATIONS (ERASE, WRITE) are bounded by the write slot's image area, and
 * that is what makes the other slot unreachable: no command a host can send
 * changes a byte outside the slot being updated, so the image the device is
 * currently able to boot cannot be damaged — by mistake, by a stale address,
 * or deliberately. The bootloader region was already unreachable.
 *
 * The bound is the slot CAPACITY, not its size, so the slot's own record
 * block at the top is excluded too: only mutation_begin() may erase it, at
 * the moment it invalidates the record. A capacity of 0 (silicon too small
 * to hold this slot) rejects everything, which is the intent. */
static int write_range_ok(uint32_t addr, uint32_t len)
{
    uint32_t base = ob_slot_base(write_slot);
    uint32_t cap = ob_slot_capacity(write_slot);

    return cap != 0 && addr >= base && addr < base + cap &&
           len != 0 && len <= base + cap - addr;
}

/* --- state transitions ------------------------------------------------ */

/* (Re-)open a session: bitmap, stream run and disarm state all reset. The
 * record state deliberately survives — it is power-cycle-scoped. */
static void session_open(void)
{
    memset(&s, 0, sizeof s);
    s.session = 1;
    s.stream = OB_STREAM_RUN;
    s.expected_next = ob_slot_base(write_slot);
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
    r = record_invalidate(write_slot);
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
    uint32_t blk = (addr - ob_slot_base(write_slot)) / OB_FLASH_ERASE_BLOCK;

    s.bitmap[blk >> 3] |= (uint8_t)(1u << (blk & 7u));
}

static int bitmap_covers(uint32_t addr, uint32_t len)
{
    uint32_t base = ob_slot_base(write_slot);
    uint32_t first = (addr - base) / OB_FLASH_ERASE_BLOCK;
    uint32_t last = (addr + len - 1 - base) / OB_FLASH_ERASE_BLOCK;
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

/* Does the sequential run cover exactly [write_base, write_base+img_len)? */
static int stream_covers(uint32_t img_len)
{
    return s.stream == OB_STREAM_RUN &&
           s.expected_next == ob_slot_base(write_slot) + img_len;
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

static void record_note_committed(uint32_t img_len, uint32_t img_crc,
                                  uint32_t generation)
{
    rec_state = OB_REC_FRESH;
    committed_img_len = img_len;
    committed_img_crc = img_crc;
    highest_gen = generation;

    /* The slot just committed now holds the highest valid generation, so it
     * is what the boot decision will pick — and the next update must target
     * the other one. This is the only place the pair moves, because COMMIT
     * is the only thing that changes which slot is bootable.
     *
     * The session's erase bitmap, sequential run and disarm flag all
     * describe the slot we have just left, so re-arm them against the new
     * target: a second update within one session then starts from the same
     * clean state a fresh HELLO would give it. The bitmap clear below is
     * load-bearing, not hygiene: bitmap indices are relative to write_slot,
     * so a bit carried across this flip would arm the same-numbered block
     * of the NEW slot and let WRITE land on flash this session never
     * erased — exactly what E_NOT_ERASED exists to stop. */
    active_slot = write_slot;
    write_slot = other_slot(active_slot);

    s.disarmed = 0;                      /* a later mutation must re-disarm */
    memset(s.bitmap, 0, sizeof s.bitmap);
    s.stream = OB_STREAM_RUN;
    s.expected_next = ob_slot_base(write_slot);
    s.crc_state = ob_crc32_init();
    s.last_addr = 0;
    s.last_len = 0;
}

/* The failed write may still have landed a complete, CRC-valid record, so
 * stop claiming the record is invalidated: the next mutation must
 * re-invalidate it or a power cut mid-rewrite could boot a torn image. */
static void record_note_write_failed(void)
{
    s.disarmed = 0;
}

/* BOOT's record gate: will the reset this is about to perform actually land
 * in an application? It answers that per state, because F26 makes the flash
 * view untrustworthy for exactly the slot we have written this power cycle
 * — after an invalidate it may still read as the old (valid) record, and
 * after a fresh COMMIT it may still read as erased.
 *
 * The A/B answer differs from the single-image one in the INVALID case.
 * There, the write slot is mid-update and unbootable, but the other slot was
 * never touched this power cycle: its record and image are exactly what the
 * reset path will find. So an interrupted update no longer strands the
 * device in the bootloader — BOOT still returns it to the previous
 * application, which is the behaviour A/B exists to provide.
 *
 * Every branch that consults flash applies the FULL boot-decision
 * validation, so an explicit BOOT can never launch an app the reset path
 * would refuse. */
static int boot_record_trusted(void)
{
    if (rec_state == OB_REC_FRESH)
        return 1;                        /* just committed the write slot */
    if (rec_state == OB_REC_FLASH)
        return ob_boot_select(0) != OB_SLOT_NONE; /* all of flash is truth */
    /* OB_REC_INVALID: ask only the slot this power cycle has not written.
     * ob_boot_select() is not usable here — it would read the write slot's
     * possibly-stale record and could answer with the slot being updated. */
    return active_slot != OB_SLOT_NONE && ob_boot_app_valid(active_slot);
}

void ob_core_init(void)
{
    memset(&s, 0, sizeof s);
    rec_state = OB_REC_FLASH;
    committed_img_len = 0;
    committed_img_crc = 0;
    /* Derived once, from a flash view nothing has disturbed since reset —
     * see the note on these two at the top of the file. With nothing
     * bootable (a factory-fresh part) slot A is the target: there is no
     * image to preserve, and starting at A keeps the common case aligned
     * with the factory image. */
    active_slot = ob_boot_select(&highest_gen);
    write_slot = (active_slot == OB_SLOT_NONE) ? OB_SLOT_A
                                               : other_slot(active_slot);
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
    /* The A/B view, added in 0.2. app_start/app_end above still describe the
     * whole region; these describe the window this session may mutate. */
    r[36] = OB_SLOT_COUNT;
    r[37] = (active_slot == OB_SLOT_NONE) ? OB_SLOT_ID_NONE
                                          : (uint8_t)active_slot;
    r[38] = (uint8_t)write_slot;
    r[39] = 0;                       /* reserved */
    put32(r + 40, ob_slot_base(write_slot));
    put32(r + 44, ob_slot_capacity(write_slot));
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
    if (!write_range_ok(addr, len))
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
    if (!write_range_ok(addr, dlen))
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
    if (!read_range_ok(addr, len))
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
    /* The slot's own record sits at its top, so an image may not reach it. */
    if (img_len > ob_slot_capacity(write_slot))
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
        c = ob_xip_crc32(ob_slot_base(write_slot), img_len);
    } else {
        /* F26: XIP over freshly-written flash may be stale — only a fully
         * sequential stream from the write base can be attested. */
        if (!stream_covers(img_len))
            return err_resp(resp, OB_CMD_COMMIT, seq, OB_E_VERIFY,
                            OB_DET_VERIFY_NONSEQ);
        c = stream_crc();
    }
    if (c != img_crc)
        return err_resp(resp, OB_CMD_COMMIT, seq, OB_E_VERIFY,
                        OB_DET_VERIFY_MISMATCH);

    /* ob_core_init captured the highest valid flash generation before any
     * mutation; successful commits then advance this RAM truth directly. */
    rec.generation = highest_gen + 1u;
    /* 0xFFFFFFFF is never stored: no later generation could outrank it. The
     * increment can also wrap 0xFFFFFFFF + 1 to 0, which ob_record_load()
     * rejects. Both ends of the counter are refused before flash is touched.
     * Unreachable in
     * practice — ~2^32 commits against a flash endurance ~10^4..10^5 erase
     * cycles — so this guard is for a hand-crafted record claiming the
     * ceiling, not for wear. E_FLASH with detail 0: no ROM error code is 0,
     * and the spec (section 6.5) names this case. */
    if (rec.generation == 0xFFFFFFFFu || rec.generation == 0u)
        return err_resp(resp, OB_CMD_COMMIT, seq, OB_E_FLASH, OB_DET_NONE);
    rec.img_len = img_len;
    rec.img_crc32 = img_crc;
    record_begin_write();
    r = ob_record_store(write_slot, &rec);      /* fills magic/rsvd/rec_crc32 */
    if (r) {
        record_note_write_failed();
        return err_resp(resp, OB_CMD_COMMIT, seq, OB_E_FLASH, (uint8_t)r);
    }
    record_note_committed(img_len, img_crc, rec.generation);
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
