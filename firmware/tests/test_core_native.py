"""Host-native tests for the OpenBoot portable core (real C sources compiled
against the simulated-flash mock port; see core_host/).

Coverage here is checked by MUTATION: neutralise a guard in boot_core.c or
boot_decision.c and a test must fail. A guard whose removal leaves this suite
green is either untested, or reached only by a test that gets there down
another path - the shape behind every "passes for the wrong reason" defect
found so far.

Six guards survive that treatment on purpose, and are listed so the next sweep
does not re-derive them:

  boot_core.c  boot_record_trusted, the OB_REC_FLASH branch
      Equivalent to the branch below it in the state that selects it:
      active_slot was set from ob_boot_select() and nothing has written flash,
      so both answer the same. Not a coverage gap.

  boot_core.c  do_write, the mutation_begin() result check
      Unreachable. WRITE requires a block marked in the erase bitmap, bits are
      set only by a successful ERASE, and ERASE disarms first - so by the time
      any WRITE is accepted the record is already invalidated and
      mutation_begin() returns 0 without touching flash.

  boot_core.c  handle_frame, the first length check
      Redundant with the second one for the RESPONSE: either alone refuses a
      runt (test_a_runt_frame_is_ignored fails only when both are gone). Its
      own contribution is not reading header bytes that were never received,
      which cannot be seen here because the harness always passes a full-size
      buffer.

  boot_decision.c  ob_record_load / ob_record_store, slot >= OB_SLOT_COUNT
      Defensive against a caller bug. Neither function is exported to the
      harness, so no test can pass an out-of-range slot.

  boot_decision.c  ob_record_load, the capacity == 0 early return
      Redundant for the ANSWER - the img_len-against-capacity check below
      refuses the same records. It exists to avoid READING a record address
      that lands beyond a smaller die's flash, and sim_flash is always the
      full build size, so the hazard cannot be modelled.
"""
import random
import zlib

import pytest

from ob_native import (
    ACT_NONE, ACT_RESET, APP_START, BLOCK, BOOTREQ_MAGIC,
    CMD_BOOT, CMD_COMMIT, CMD_CRC, CMD_ERASE, CMD_HELLO, CMD_READ, CMD_WRITE,
    DET_ALIGN, DET_MISMATCH, DET_NONSEQ, DET_NORECORD, DET_RANGE,
    E_ADDR, E_ARG, E_CMD, E_FLASH, E_LEN, E_NOT_ERASED, E_PROTO, E_STATE,
    E_VERIFY, MAX_WRITE, OB_HELLO_RESP_LEN, OB_PROTO_MAJOR, OB_PROTO_MINOR,
    OB_SLOT_ID_NONE, OK, RECORD_MAGIC, RECORD_SIZE, RSVD,
    frame, get_device, load_golden,
)

GOLDEN = load_golden()
HELLO_PAYLOAD = b"OBP1" + bytes([OB_PROTO_MAJOR, OB_PROTO_MINOR])


def u32(v):
    return v.to_bytes(4, "little")


def parse(resp):
    """Validate framing/CRC of a response, return (cmd, seq, payload)."""
    assert len(resp) >= 8
    n = resp[2]
    assert len(resp) == 8 + n
    assert int.from_bytes(resp[4 + n:8 + n], "little") == zlib.crc32(resp[:4 + n])
    assert resp[3] == 0
    return resp[0], resp[1], bytes(resp[4:4 + n])


def send(dev, cmd, seq, payload=b""):
    resp, act = dev.frame(frame(cmd, seq, payload))
    rcmd, rseq, pl = parse(resp)
    assert rseq == seq
    return rcmd, pl, act


def cmd_ok(dev, cmd, seq, payload=b""):
    rcmd, pl, act = send(dev, cmd, seq, payload)
    assert rcmd == (cmd | 0x80)
    assert pl[0] == OK, f"expected OK, got {pl.hex()}"
    return pl, act


def cmd_err(dev, cmd, seq, payload, status, detail=None):
    rcmd, pl, act = send(dev, cmd, seq, payload)
    assert rcmd == (cmd | 0x80)
    assert act == ACT_NONE
    assert pl[0] == status, f"expected status {status:#x}, got {pl.hex()}"
    if detail is not None:
        assert pl[1] == detail
    return pl


def hello(dev, seq=0):
    rcmd, pl, act = send(dev, CMD_HELLO, seq, HELLO_PAYLOAD)
    assert rcmd == 0x81 and pl[0] == OK and act == ACT_NONE
    return pl


def erase(dev, addr, length, seq=1):
    cmd_ok(dev, CMD_ERASE, seq, u32(addr) + u32(length))


def write(dev, addr, data, seq=2):
    cmd_ok(dev, CMD_WRITE, seq, u32(addr) + data)


def commit(dev, image, seq=3):
    return cmd_ok(dev, CMD_COMMIT, seq,
                  u32(len(image)) + u32(zlib.crc32(image)))


def slot_view(pl):
    """The A/B fields of a HELLO payload: (active, write, base, capacity)."""
    return (pl[37], pl[38],
            int.from_bytes(pl[40:44], "little"),
            int.from_bytes(pl[44:48], "little"))


def write_target(dev, seq=0):
    """Open a session and return (slot, base, capacity) it will accept."""
    _active, slot, base, cap = slot_view(hello(dev, seq))
    return slot, base, cap


def full_update(dev, image, whole_slot=False):
    """HELLO + ERASE + sequential WRITE + COMMIT into whichever slot the
    device says it is willing to write. Returns that slot's index — after
    the COMMIT it is the ACTIVE slot, and the next update targets the other
    one. Caller resets first."""
    slot, base, cap = write_target(dev)
    seq = 1
    if whole_slot:
        a = base
        while a < base + cap:
            length = min(0x8000, base + cap - a)    # host chunks <= 32 KiB
            erase(dev, a, length, seq & 0xFF)
            seq += 1
            a += length
    else:
        blocks = (len(image) + BLOCK - 1) // BLOCK
        erase(dev, base, blocks * BLOCK, seq & 0xFF)
        seq += 1
    for off in range(0, len(image), MAX_WRITE):
        write(dev, base + off, image[off:off + MAX_WRITE], seq & 0xFF)
        seq += 1
    commit(dev, image, seq & 0xFF)
    return slot


def swd_install(dev, image, slot=0):
    """Model an image put in a slot out of band (SWD, factory image): the
    bytes are in flash with NO record and nothing has gone through the flash
    controller this power cycle, which is exactly the bless precondition."""
    dev.reset()
    dev.write_flash(dev.slot_base(slot), image)


def expected_record(image, generation=1):
    """The 32-byte OBR2 record a COMMIT of `image` should leave in its slot.

    generation defaults to 1 because every mutation erases the target slot's
    record first, so a fresh commit always claims the lowest generation."""
    body = (u32(RECORD_MAGIC) + u32(generation) + u32(len(image)) +
            u32(zlib.crc32(image)) + bytes(RSVD))
    return body + u32(zlib.crc32(body))


@pytest.fixture(params=["ch57x", "ch59x"])
def dev(request):
    d = get_device(request.param)
    d.reset()
    return d


@pytest.fixture()
def dev57():
    d = get_device("ch57x")
    d.reset()
    return d


@pytest.fixture()
def dev59():
    d = get_device("ch59x")
    d.reset()
    return d


# --- CRC32 --------------------------------------------------------------

def test_crc32_vs_zlib(dev):
    assert GOLDEN["crc_check"] == u32(zlib.crc32(b"123456789"))
    rng = random.Random(0xC7C)
    for length in [0, 1, 2, 3, 4, 7, 33, 256, 4096]:
        buf = bytes(rng.randrange(256) for _ in range(length))
        assert dev.crc32_native(buf) == zlib.crc32(buf)


# --- golden vectors -----------------------------------------------------

def test_golden_hello(dev):
    resp, act = dev.frame(GOLDEN["hello_req"])
    assert act == ACT_NONE
    if dev.family == "ch59x":
        assert resp == GOLDEN["hello_resp_ch592_usb"]
    _, _, pl = parse(resp)
    assert len(pl) == OB_HELLO_RESP_LEN
    assert pl[0] == OK
    assert (pl[1], pl[2]) == (OB_PROTO_MAJOR, OB_PROTO_MINOR)
    assert pl[3] == 9                                     # chip_rev
    # Deliberate literal: a version bump must be acknowledged here, not
    # inherited silently. Bump alongside boot_core.h's OB_BL_VERSION.
    assert int.from_bytes(pl[4:6], "little") == 0x000B    # bl_version v0.11
    assert pl[6] == dev.family_id
    assert pl[7] == 1                                     # transport USB
    assert int.from_bytes(pl[8:12], "little") == APP_START
    assert int.from_bytes(pl[12:16], "little") == dev.app_end
    assert int.from_bytes(pl[16:20], "little") == BLOCK
    assert int.from_bytes(pl[20:22], "little") == 256     # write_page
    assert pl[22] == 4                                    # align
    assert pl[23] == MAX_WRITE
    assert int.from_bytes(pl[24:28], "little") == dev.features
    assert int.from_bytes(pl[28:36], "little") == 0x0123456789ABCDEF
    # 0.2: the A/B view. Nothing is committed on a fresh harness, so no
    # slot is active and the write target is A at the app base.
    active, wslot, base, cap = slot_view(pl)
    assert (active, wslot) == (OB_SLOT_ID_NONE, 0)
    assert (base, cap) == (dev.slot_base(0), dev.slot_capacity(0))
    assert base == APP_START
    assert pl[36] == 2 and pl[39] == 0                    # count, reserved


def test_golden_session(dev):
    """Drive the golden request vectors through a real session."""
    resp, _ = dev.frame(GOLDEN["hello_req"])
    if dev.family == "ch59x":
        assert resp == GOLDEN["hello_resp_ch592_usb"]
    resp, act = dev.frame(GOLDEN["erase_req"])            # 0x2000 += 0x1000
    assert resp == GOLDEN["erase_ok"] and act == ACT_NONE
    resp, act = dev.frame(GOLDEN["write_req"])            # 8 B at 0x2000
    assert resp == GOLDEN["write_ok"] and act == ACT_NONE
    assert dev.flash_read(0x2000, 8) == bytes.fromhex("deadbeefcafebabe")
    erase(dev, 0x2000, 2 * BLOCK, seq=0x10)
    resp, act = dev.frame(GOLDEN["write_req_max"])        # 48 B at 0x3000
    assert resp == frame(0x83, 0x06, bytes([OK])) and act == ACT_NONE
    assert dev.flash_read(0x3000, 48) == bytes(range(48))
    # commit_req: img_len 0x9C40, crc 0x12345678 — cannot attest.
    resp, act = dev.frame(GOLDEN["commit_req"])
    if dev.features:                     # CRC_LIVE: XIP content mismatch
        assert resp == frame(0x85, 0x04, bytes([E_VERIFY, DET_MISMATCH]))
    else:                                # CH57x: 0x2000 then 0x3000 = nonseq
        assert resp == GOLDEN["commit_err_nonseq"]
    resp, act = dev.frame(GOLDEN["boot_req"])             # mode 0, no record
    assert resp == frame(0x86, 0x05, bytes([E_VERIFY, DET_NORECORD]))
    assert act == ACT_NONE
    assert dev.violations() == 0


def test_golden_write_before_erase(dev):
    hello(dev)
    resp, act = dev.frame(GOLDEN["write_req"])
    assert resp == GOLDEN["write_err_not_erased"] and act == ACT_NONE
    assert dev.flash_read(0x2000, 8) == dev.erased(0x2000, 8)


def test_golden_crc_cmd(dev):
    hello(dev)
    resp, _ = dev.frame(GOLDEN["crc_req"])                # 0x2000 len 0x2000
    expect = zlib.crc32(dev.erased(0x2000, 0x2000))
    assert resp == frame(0x84, 0x03, bytes([OK]) + u32(expect))


def test_golden_frame_err(dev):
    bad = bytearray(GOLDEN["hello_req"])
    bad[1] = 9                           # patch seq: CRC now stale
    resp, act = dev.frame(bytes(bad))
    assert resp == GOLDEN["frame_err"] and act == ACT_NONE


def test_min_frame_unknown_cmd(dev):
    rcmd, _, pl = parse(dev.frame(GOLDEN["min_frame"])[0])
    assert rcmd == 0xFF and pl == bytes([E_CMD, 0])       # 0x7F | 0x80


# --- full update flow ---------------------------------------------------

def test_full_update_flow(dev):
    rng = random.Random(0xF10)
    image = bytes(rng.randrange(256) for _ in range(10240))
    slot = full_update(dev, image, whole_slot=True)
    assert dev.flash_read(dev.slot_base(slot), len(image)) == image
    assert dev.record_raw(slot) == expected_record(image)
    _pl, act = cmd_ok(dev, CMD_BOOT, 0x50, bytes([0]))
    # Reset-to-launch on every family: no stay magic, the post-reset boot
    # decision (coherent XIP) is the single launch authority.
    assert act == ACT_RESET
    assert dev.violations() == 0
    dev.power_cycle()
    assert dev.boot_decide() == 0        # launches the new app


def test_usb_report_padding(dev):
    """USB passes the whole zero-padded 64 B report; core trims by len."""
    f = frame(CMD_HELLO, 7, HELLO_PAYLOAD)
    resp, act = dev.frame(f + bytes(64 - len(f)))
    rcmd, rseq, pl = parse(resp)
    assert (rcmd, rseq, pl[0]) == (0x81, 7, OK) and act == ACT_NONE


def test_boot_requires_session(dev):
    """IDLE accepts only HELLO: pre-handshake BOOT frames (both modes)
    are E_STATE and must neither jump nor reset. After HELLO they work."""
    image = bytes(range(1, 65))
    full_update(dev, image)
    dev.power_cycle()                    # back to IDLE, no HELLO
    cmd_err(dev, CMD_BOOT, 1, bytes([0]), E_STATE, 0)
    cmd_err(dev, CMD_BOOT, 2, bytes([1]), E_STATE, 0)
    hello(dev, 3)
    _pl, act = cmd_ok(dev, CMD_BOOT, 4, bytes([0]))
    assert act == ACT_RESET
    # The bootreq magic is the ONLY observable mode-0/mode-1 difference:
    # mode 0 must not write it (so the reset launches the app) ...
    assert dev.get_bootreq() != BOOTREQ_MAGIC
    _pl, act = cmd_ok(dev, CMD_BOOT, 5, bytes([1]))
    assert act == ACT_RESET
    # ... and mode 1 must (so the reset stays in the bootloader).
    assert dev.get_bootreq() == BOOTREQ_MAGIC


# --- regressions: addressing, lengths, state ---------------------------

def test_write_before_erase(dev):
    hello(dev)
    cmd_err(dev, CMD_WRITE, 1, u32(APP_START) + bytes(8), E_NOT_ERASED, 0)
    assert dev.flash_read(APP_START, 8) == dev.erased(APP_START, 8)


def test_bootloader_region_untouchable(dev):
    hello(dev)
    cmd_err(dev, CMD_ERASE, 1, u32(0) + u32(BLOCK), E_ADDR, DET_RANGE)
    cmd_err(dev, CMD_WRITE, 2, u32(0x0FFC) + bytes(8), E_ADDR, DET_RANGE)
    cmd_err(dev, CMD_CRC, 3, u32(0) + u32(0x1000), E_ADDR, DET_RANGE)
    assert dev.flash_read(0, 0x1000) == dev.erased(0, 0x1000)


def test_range_beyond_app_end(dev):
    hello(dev)
    end = dev.app_end
    cmd_err(dev, CMD_ERASE, 1, u32(end) + u32(BLOCK), E_ADDR, DET_RANGE)
    cmd_err(dev, CMD_ERASE, 2, u32(APP_START) + u32(end), E_ADDR, DET_RANGE)
    cmd_err(dev, CMD_WRITE, 3, u32(end - 4) + bytes(8), E_ADDR, DET_RANGE)
    cmd_err(dev, CMD_WRITE, 4, u32(end) + bytes(4), E_ADDR, DET_RANGE)
    cmd_err(dev, CMD_CRC, 5, u32(end - 4) + u32(8), E_ADDR, DET_RANGE)
    cmd_err(dev, CMD_ERASE, 6, u32(APP_START) + u32(0), E_ADDR, DET_RANGE)
    cmd_err(dev, CMD_CRC, 7, u32(APP_START) + u32(0), E_ADDR, DET_RANGE)


def test_misalignment(dev):
    hello(dev)
    cmd_err(dev, CMD_ERASE, 1, u32(0x1800) + u32(BLOCK), E_ADDR, DET_ALIGN)
    cmd_err(dev, CMD_ERASE, 2, u32(APP_START) + u32(0x800), E_ADDR, DET_ALIGN)
    cmd_err(dev, CMD_WRITE, 3, u32(0x1002) + bytes(8), E_ADDR, DET_ALIGN)
    cmd_err(dev, CMD_CRC, 4, u32(0x1002) + u32(8), E_ADDR, DET_ALIGN)
    cmd_err(dev, CMD_CRC, 5, u32(APP_START) + u32(6), E_ADDR, DET_ALIGN)


def test_payload_len_violations(dev):
    hello(dev)
    cmd_err(dev, CMD_HELLO, 1, HELLO_PAYLOAD + b"x", E_LEN, 0)
    cmd_err(dev, CMD_ERASE, 2, u32(APP_START), E_LEN, 0)
    cmd_err(dev, CMD_WRITE, 3, u32(APP_START) + bytes(2), E_LEN, 0)
    cmd_err(dev, CMD_WRITE, 4, u32(APP_START) + bytes(50), E_LEN, 0)
    cmd_err(dev, CMD_WRITE, 5, u32(APP_START), E_LEN, 0)
    cmd_err(dev, CMD_CRC, 6, u32(APP_START) + u32(4) + b"x", E_LEN, 0)
    cmd_err(dev, CMD_COMMIT, 7, u32(8), E_LEN, 0)
    cmd_err(dev, CMD_BOOT, 8, b"", E_LEN, 0)
    cmd_err(dev, CMD_BOOT, 9, bytes([0, 0]), E_LEN, 0)


def test_commit_img_len_violations(dev):
    hello(dev)
    cmd_err(dev, CMD_COMMIT, 1, u32(0) + u32(0), E_LEN, 0)
    cmd_err(dev, CMD_COMMIT, 2, u32(6) + u32(0), E_LEN, 0)
    too_big = dev.app_end - APP_START + 4
    cmd_err(dev, CMD_COMMIT, 3, u32(too_big) + u32(0), E_ADDR, DET_RANGE)


def test_unknown_opcode(dev):
    cmd_err(dev, 0x40, 1, b"", E_CMD, 0)                  # idle
    cmd_err(dev, CMD_READ, 2, b"", E_CMD, 0)              # READ off in v1
    hello(dev)
    cmd_err(dev, 0x40, 3, b"", E_CMD, 0)                  # session
    cmd_err(dev, CMD_READ, 4, b"", E_CMD, 0)


def test_state_before_hello(dev):
    cmd_err(dev, CMD_ERASE, 1, u32(APP_START) + u32(BLOCK), E_STATE, 0)
    cmd_err(dev, CMD_WRITE, 2, u32(APP_START) + bytes(8), E_STATE, 0)
    cmd_err(dev, CMD_CRC, 3, u32(APP_START) + u32(8), E_STATE, 0)
    cmd_err(dev, CMD_COMMIT, 4, u32(8) + u32(0), E_STATE, 0)


def test_bad_flags(dev):
    resp, act = dev.frame(frame(CMD_HELLO, 5, HELLO_PAYLOAD, flags=1))
    rcmd, rseq, pl = parse(resp)
    assert (rcmd, rseq) == (0x81, 5) and pl == bytes([E_ARG, 0])
    assert act == ACT_NONE


def test_hello_rejects(dev):
    cmd_err(dev, CMD_HELLO, 1,
            b"OBP1" + bytes([OB_PROTO_MAJOR + 1, OB_PROTO_MINOR]), E_PROTO, 0)
    # pre-1.0 (major 0): the minor must match exactly too, in both
    # directions — an older host is refused as firmly as a newer one
    cmd_err(dev, CMD_HELLO, 2,
            b"OBP1" + bytes([OB_PROTO_MAJOR, OB_PROTO_MINOR + 1]), E_PROTO, 0)
    cmd_err(dev, CMD_HELLO, 3,
            b"OBP1" + bytes([OB_PROTO_MAJOR, OB_PROTO_MINOR - 1]), E_PROTO, 0)
    cmd_err(dev, CMD_HELLO, 4, b"OBPX" + HELLO_PAYLOAD[4:], E_ARG, 0)
    # none of them opened a session
    cmd_err(dev, CMD_ERASE, 5, u32(APP_START) + u32(BLOCK), E_STATE, 0)


def test_boot_bad_mode(dev):
    cmd_err(dev, CMD_BOOT, 1, bytes([2]), E_STATE, 0)     # no session yet
    hello(dev)
    cmd_err(dev, CMD_BOOT, 2, bytes([2]), E_ARG, 0)


def test_boot_no_record(dev):
    cmd_err(dev, CMD_BOOT, 1, bytes([0]), E_STATE, 0)     # no session yet
    hello(dev)
    cmd_err(dev, CMD_BOOT, 2, bytes([0]), E_VERIFY, DET_NORECORD)


def test_commit_wrong_crc(dev):
    image = bytes(range(4, 100))         # 96 B
    hello(dev)
    erase(dev, APP_START, BLOCK)
    write(dev, APP_START, image[:48], 2)
    write(dev, APP_START + 48, image[48:], 3)
    bad = zlib.crc32(image) ^ 1
    cmd_err(dev, CMD_COMMIT, 4, u32(len(image)) + u32(bad),
            E_VERIFY, DET_MISMATCH)
    assert dev.record_raw() == dev.erased(dev.slot_record_addr(0), RECORD_SIZE)          # still invalidated
    cmd_err(dev, CMD_BOOT, 5, bytes([0]), E_VERIFY, DET_NORECORD)


def test_errors_never_change_state(dev):
    hello(dev)
    erase(dev, APP_START, BLOCK)
    cmd_err(dev, CMD_WRITE, 2, u32(0x1002) + bytes(8), E_ADDR, DET_ALIGN)
    cmd_err(dev, 0x40, 3, b"", E_CMD, 0)
    write(dev, APP_START, bytes(range(48)), 4)            # session survives
    assert dev.flash_read(APP_START, 48) == bytes(range(48))


def test_rehello_clears_bitmap(dev):
    hello(dev)
    erase(dev, APP_START, BLOCK)
    write(dev, APP_START, bytes(8), 2)
    hello(dev, seq=3)
    cmd_err(dev, CMD_WRITE, 4, u32(APP_START) + bytes(8), E_NOT_ERASED, 0)
    # and the stream CRC restarted: a fresh sequential update still attests
    image = bytes(range(64, 128))
    erase(dev, APP_START, BLOCK, 5)
    write(dev, APP_START, image[:48], 6)
    write(dev, APP_START + 48, image[48:], 7)
    commit(dev, image, 8)
    assert dev.record_raw() == expected_record(image)


def test_undecodable_frames_get_no_response(dev):
    for junk in [b"", b"\x01", bytes(7),
                 bytes([1, 0, 57, 0]) + bytes(60),        # len > 56
                 frame(CMD_HELLO, 1, HELLO_PAYLOAD)[:10]]:  # truncated
        resp, act = dev.frame(junk)
        assert resp == b"" and act == ACT_NONE


# --- stream CRC / F26 semantics ----------------------------------------

def test_crc_cmd_after_write(dev):
    image = bytes(range(100, 228))       # 128 B
    hello(dev)
    erase(dev, APP_START, BLOCK)
    write(dev, APP_START, image[:48], 2)
    write(dev, APP_START + 48, image[48:96], 3)
    write(dev, APP_START + 96, image[96:], 4)
    if dev.family == "ch57x":
        dev.set_f26(0)                   # XIP truthful for this check only
    pl, _ = cmd_ok(dev, CMD_CRC, 5, u32(APP_START) + u32(128))
    assert int.from_bytes(pl[1:5], "little") == zlib.crc32(image)


def test_reset_restores_ch57x_f26_default(dev57):
    image = bytes(range(1, 49))
    dev57.set_f26(0)
    dev57.reset()
    hello(dev57)
    erase(dev57, APP_START, BLOCK)
    write(dev57, APP_START, image, 2)

    pl, _ = cmd_ok(dev57, CMD_CRC, 3, u32(APP_START) + u32(len(image)))
    observed = int.from_bytes(pl[1:5], "little")
    assert observed == zlib.crc32(dev57.erased(APP_START, len(image)))
    assert observed != zlib.crc32(image)


def test_ch57x_a_broken_run_is_refused_even_when_it_covers_the_image(dev57):
    """Isolates the POISONING from the length arithmetic.

    An earlier version wrote two chunks out of order and asserted NONSEQ. That
    passed with the poisoning deleted, because the shuffled run never reached
    img_len and stream_covers() refused it anyway - so it proved nothing about
    the poisoning it was named for.

    Here the run does cover img_len, and the folded bytes are exactly the
    image, so the ONLY thing left to refuse it is that a same-range rewrite
    with different bytes broke the run. Delete the poisoning and this commits."""
    image = bytes(range(16, 80))
    claim = u32(len(image)) + u32(zlib.crc32(image))

    dev57.reset()
    hello(dev57)
    erase(dev57, APP_START, BLOCK)
    write(dev57, APP_START, image[:48], 2)               # folds
    over = bytes(b & 0xF0 for b in image[:48])           # same range, further 1->0
    write(dev57, APP_START, over, 3)                     # accepted by flash, breaks the run
    write(dev57, APP_START + 48, image[48:], 4)          # folds; the run now spans img_len
    cmd_err(dev57, CMD_COMMIT, 5, claim, E_VERIFY, DET_NONSEQ)
    assert dev57.record_raw() == dev57.erased(dev57.slot_record_addr(0), RECORD_SIZE)


def test_ch57x_a_stream_shorter_than_the_claim_is_refused(dev57):
    """Sequential but shorter than img_len.

    The control commits the 48 bytes that were actually written, which is what
    makes the refusal below about the LENGTH CLAIMED rather than about folding
    having silently done nothing: with folding deleted the cursor never moves,
    every commit is short, and the negative case passes for the wrong reason."""
    body = bytes(48)

    dev57.reset()
    hello(dev57)
    erase(dev57, APP_START, BLOCK)
    write(dev57, APP_START, body, 2)
    cmd_ok(dev57, CMD_COMMIT, 3, u32(48) + u32(zlib.crc32(body)))

    dev57.reset()
    hello(dev57)
    erase(dev57, APP_START, BLOCK)
    write(dev57, APP_START, body, 2)
    cmd_err(dev57, CMD_COMMIT, 3, u32(96) + u32(zlib.crc32(bytes(96))),
            E_VERIFY, DET_NONSEQ)


def test_ch57x_stream_commit_with_f26_poison(dev57):
    """Full sequential update attests via stream CRC while XIP is stale."""
    rng = random.Random(0xF26)
    image = bytes(rng.randrange(256) for _ in range(4096 + 48))
    full_update(dev57, image)            # f26 poisoning active
    assert dev57.record_raw() == expected_record(image)
    assert dev57.flash_read(APP_START, len(image)) == image
    assert dev57.violations() == 0


def test_ch57x_bless_via_xip_despite_f26(dev57):
    """Zero-write session: no blocks dirtied, XIP coherent, bless works."""
    image = bytes(range(1, 129))
    swd_install(dev57, image, slot=0)
    slot, base, _cap = write_target(dev57)
    assert (slot, base) == (0, APP_START)   # nothing bootable: the target is A
    _pl, act = commit(dev57, image, 1)      # write_count == 0 -> XIP path
    assert act == ACT_NONE
    assert dev57.record_raw(0) == expected_record(image, generation=1)
    assert dev57.violations() == 0


def test_bless_of_the_inactive_slot_outranks_the_running_image(dev):
    """The A/B bless: an image installed out of band into the INACTIVE slot
    has to claim a generation above the record still describing the running
    one, or the boot decision would keep choosing the old slot."""
    old = bytes(range(1, 65))
    assert full_update(dev, old) == 0
    dev.power_cycle()
    slot, base, _cap = write_target(dev)
    assert slot == 1                        # A is active, so B is the target
    new = bytes(range(10, 138))
    dev.write_flash(base, new)
    commit(dev, new, 1)
    assert dev.record_raw(1) == expected_record(new, generation=2)
    assert dev.record_raw(0) == expected_record(old, generation=1)
    dev.power_cycle()
    assert dev.boot_select() == 1


def test_commit_retry_is_idempotent(dev):
    """An exact replay after a lost OK must not rewrite the boot record."""
    image = bytes(range(1, 97))
    full_update(dev, image)
    after_first = dev.op_total()

    commit(dev, image, 0x40)
    assert dev.op_total() == after_first
    assert dev.record_raw() == expected_record(image)

    hello(dev, 0x41)                    # replay state survives a new session
    commit(dev, image, 0x42)
    assert dev.op_total() == after_first


def test_bless_commit_retry_is_idempotent(dev):
    """The CH57x failure mode: record writes dirty XIP after a bless."""
    image = bytes(range(1, 97))
    swd_install(dev, image, slot=0)
    write_target(dev)
    commit(dev, image, 1)
    after_bless = dev.op_total()

    # The COMMIT moved the write target to the other slot. The replay cache
    # keys on the tuple that reached flash, not on the current target, so a
    # retry is still acknowledged without touching the controller.
    commit(dev, image, 2)
    assert dev.op_total() == after_bless
    hello(dev, 3)
    commit(dev, image, 4)
    assert dev.op_total() == after_bless


def test_commit_replay_requires_exact_tuple(dev):
    image = bytes(range(1, 97))
    full_update(dev, image)
    before = dev.op_total()
    # Not the committed tuple, so it must be re-attested — against the
    # CURRENT write slot, which the commit flip just moved to the empty one.
    # It fails either way and touches no flash; the detail differs only
    # because ch59x may attest straight from XIP and ch57x may not.
    cmd_err(dev, CMD_COMMIT, 0x40,
            u32(len(image)) + u32(zlib.crc32(image) ^ 1), E_VERIFY,
            DET_MISMATCH if dev.family == "ch59x" else DET_NONSEQ)
    assert dev.op_total() == before


def test_mutation_invalidates_commit_replay(dev):
    image = bytes(range(1, 97))
    slot = full_update(dev, image)
    erase(dev, dev.slot_base(1 - slot), BLOCK, seq=0x40)   # the new target
    before = dev.op_total()
    cmd_err(dev, CMD_COMMIT, 0x41,
            u32(len(image)) + u32(zlib.crc32(image)), E_VERIFY)
    assert dev.op_total() == before


def test_power_cycle_forgets_commit_replay_cache(dev):
    image = bytes(range(1, 97))
    full_update(dev, image)
    dev.power_cycle()
    slot, base, _cap = write_target(dev)
    dev.write_flash(base, image)        # stage the new target out of band
    before = dev.op_total()
    commit(dev, image, 1)               # same tuple as before the cycle
    # Two mutating ops, not one: storing a record erases its block before
    # writing it, because flash only clears bits. A surviving cache would
    # have acknowledged this for free.
    assert dev.op_total() == before + 2
    assert dev.record_raw(slot) == expected_record(image, generation=2)


def test_ch59x_nonseq_commit_ok(dev59):
    """CRC_LIVE: XIP is authoritative, write order is irrelevant."""
    image = bytes(range(16, 80))
    hello(dev59)
    erase(dev59, APP_START, BLOCK)
    write(dev59, APP_START + 48, image[48:], 2)
    write(dev59, APP_START, image[:48], 3)
    commit(dev59, image, 4)
    assert dev59.record_raw() == expected_record(image)


# --- F26 record-view staleness (power-cycle-scoped gates) ----------------

def test_boot_rejected_when_the_only_slot_is_mid_update(dev):
    """From the first mutation until a successful COMMIT the write slot is
    unbootable, and on CH57x its record still reads (F26) as whatever stood
    there before the invalidate. With nothing else to fall back on — a
    factory part being flashed for the first time — BOOT must refuse."""
    _slot, base, _cap = write_target(dev)
    erase(dev, base, BLOCK, seq=1)               # invalidates the record
    cmd_err(dev, CMD_BOOT, 2, bytes([0]), E_VERIFY, DET_NORECORD)


def test_boot_mid_update_falls_back_to_the_untouched_slot(dev):
    """A/B's headline property. The same window as above, on a device that
    has a previous image: mutations are confined to the INACTIVE slot, so
    the active one is still exactly what the reset path will find and BOOT
    returns to it. Beginning an update no longer strands the device."""
    image = bytes(range(64))
    slot = full_update(dev, image)
    dev.power_cycle()
    wslot, base, _cap = write_target(dev)
    assert wslot != slot
    erase(dev, base, BLOCK, seq=1)               # invalidates the write slot
    _pl, act = cmd_ok(dev, CMD_BOOT, 2, bytes([0]))
    assert act == ACT_RESET
    dev.power_cycle()
    assert dev.boot_select() == slot
    assert dev.flash_read(dev.slot_base(slot), len(image)) == image


def test_ch57x_boot_after_commit_despite_stale_record_view(dev57):
    """A successful COMMIT is RAM truth: the stale record view (still the
    pre-update state under F26) must not block the subsequent BOOT — which
    on dirty CH57x flash is a reset-to-launch, not a direct jump."""
    image = bytes(range(48))
    full_update(dev57, image)                    # f26 poisoning active
    _pl, act = cmd_ok(dev57, CMD_BOOT, 0x60, bytes([0]))
    assert act == ACT_RESET                      # never execute stale XIP
    assert dev57.get_bootreq() != BOOTREQ_MAGIC  # no stay magic: boots app
    dev57.power_cycle()
    assert dev57.boot_decide() == 0              # launches the new app


def test_ch57x_bless_then_boot_resets(dev57):
    """Even a bless COMMIT writes the record page (a controller write), so
    the conservative reset-to-launch applies after it too."""
    image = bytes(range(1, 65))
    swd_install(dev57, image, slot=0)
    write_target(dev57)
    commit(dev57, image, 1)                      # bless: no app mutation
    _pl, act = cmd_ok(dev57, CMD_BOOT, 2, bytes([0]))
    assert act == ACT_RESET


def test_ch57x_clean_boot_resets_to_launch(dev57):
    """Even with clean, coherent flash BOOT is reset-to-launch: one launch
    authority (the boot decision), no family-divergent paths."""
    image = bytes(range(1, 65))
    full_update(dev57, image)
    dev57.power_cycle()                          # committed history, clean
    hello(dev57)
    _pl, act = cmd_ok(dev57, CMD_BOOT, 1, bytes([0]))
    assert act == ACT_RESET
    assert dev57.get_bootreq() != BOOTREQ_MAGIC  # no stay magic: boots app
    dev57.power_cycle()
    assert dev57.boot_decide() == 0


def test_erase_of_streamed_block_poisons_commit(dev57):
    """Re-erasing a block already folded into the stream CRC must void the
    stream: the CRC describes bytes that are no longer in flash."""
    image = bytes(range(48))
    hello(dev57)
    erase(dev57, APP_START, BLOCK, seq=1)
    write(dev57, APP_START, image, 2)
    erase(dev57, APP_START, BLOCK, seq=3)        # bytes gone, CRC stale
    cmd_err(dev57, CMD_COMMIT, 4, u32(48) + u32(zlib.crc32(image)),
            E_VERIFY, DET_NONSEQ)
    assert dev57.record_raw() == dev57.erased(dev57.slot_record_addr(0), RECORD_SIZE)


def test_retry_with_different_bytes_poisons_commit(dev57):
    """A same-range 'retry' carrying different bytes is not a retry: flash
    now differs from the stream CRC, so COMMIT must refuse."""
    first = bytes([0xFF] * 48)
    second = bytes([0x00] * 48)                  # 1->0 only: write succeeds
    hello(dev57)
    erase(dev57, APP_START, BLOCK, seq=1)
    write(dev57, APP_START, first, 2)
    write(dev57, APP_START, second, 3)           # addr/len match, data not
    cmd_err(dev57, CMD_COMMIT, 4, u32(48) + u32(zlib.crc32(first)),
            E_VERIFY, DET_NONSEQ)
    assert dev57.violations() == 0


def test_exact_retry_still_accepted(dev57):
    """The genuine lost-response retry (byte-identical) keeps working."""
    image = bytes(range(48))
    hello(dev57)
    erase(dev57, APP_START, BLOCK, seq=1)
    write(dev57, APP_START, image, 2)
    write(dev57, APP_START, image, 3)            # identical re-send
    commit(dev57, image, 4)
    assert dev57.record_raw() == expected_record(image)


def test_partial_erase_failure_still_poisons_stream(dev57):
    """Multi-block ERASE over streamed data: first block erases (bytes
    gone), second block fails. The early E_FLASH return must not leave
    the stream attestable."""
    image = bytes(range(48))
    hello(dev57)
    erase(dev57, APP_START, 2 * BLOCK, seq=1)
    write(dev57, APP_START, image, 2)
    dev57.set_fail_after(1)                      # 1st erase ok, 2nd fails
    cmd_err(dev57, CMD_ERASE, 3, u32(APP_START) + u32(2 * BLOCK),
            E_FLASH, None)
    dev57.set_fail_after(-1)
    cmd_err(dev57, CMD_COMMIT, 4, u32(48) + u32(zlib.crc32(image)),
            E_VERIFY, DET_NONSEQ)


def test_failed_oos_write_still_poisons_stream(dev57):
    """An out-of-sequence write that fails mid-op (flash possibly altered)
    must poison the stream before the early return."""
    first = bytes([0xFF] * 48)
    second = bytes([0x00] * 48)
    hello(dev57)
    erase(dev57, APP_START, BLOCK, seq=1)
    write(dev57, APP_START, first, 2)
    dev57.set_fail_after(0)                      # the rewrite op fails
    cmd_err(dev57, CMD_WRITE, 3, u32(APP_START) + second, E_FLASH, None)
    dev57.set_fail_after(-1)
    cmd_err(dev57, CMD_COMMIT, 4, u32(48) + u32(zlib.crc32(first)),
            E_VERIFY, DET_NONSEQ)


def test_failed_record_write_reinvalidates_on_next_mutation(dev):
    """A COMMIT whose record write fails may still have landed a complete,
    CRC-valid record in flash (the image CRC had already passed). The
    session must therefore stop believing the record is invalidated, so the
    next mutation re-invalidates it — otherwise a power cut during the
    follow-up rewrite could boot a torn image."""
    image = bytes(range(48))
    hello(dev)
    erase(dev, APP_START, BLOCK, seq=1)          # op 1 invalidate, op 2 erase
    write(dev, APP_START, image, 2)              # op 3 write
    dev.set_fail_after(0)                        # the record write fails
    cmd_err(dev, CMD_COMMIT, 3, u32(48) + u32(zlib.crc32(image)),
            E_FLASH, None)
    dev.set_fail_after(-1)
    # Simulate the record having physically landed despite the reported
    # failure, then mutate again: the record MUST be re-invalidated.
    dev.set_record_raw(expected_record(image))
    erase(dev, APP_START, BLOCK, seq=4)
    assert dev.record_raw() == dev.erased(dev.slot_record_addr(0), RECORD_SIZE), \
        "second mutation did not re-invalidate a possibly-landed record"


def test_boot_applies_full_app_validation(dev):
    """Explicit BOOT on a clean device must apply the same checks as the
    reset-time boot decision: a valid record over an erased app (e.g. the
    app was wiped out-of-band) must not be jumped into."""
    image = bytes(range(1, 49))
    dev.reset()
    dev.set_record_raw(expected_record(image))   # valid record, erased app
    hello(dev)
    cmd_err(dev, CMD_BOOT, 1, bytes([0]), E_VERIFY, DET_NORECORD)


def _forged_record(img_len, image, generation=1):
    """CRC-valid record body with an arbitrary (possibly bogus) img_len."""
    body = (u32(RECORD_MAGIC) + u32(generation) + u32(img_len) +
            u32(zlib.crc32(image)) + bytes(RSVD))
    return body + u32(zlib.crc32(body))


def test_record_geometry_gates_boot_decision(dev):
    """A record with valid magic+rec_crc32 but impossible img_len geometry
    (zero, misaligned, oversized) must never validate — PROTOCOL section
    9.1's geometry clause is always-on, not part of the optional image-CRC
    check. Reset-path variant."""
    image = bytes(range(1, 49))
    full_update(dev, image)
    dev.power_cycle()
    for bad_len in (0, 6, dev.app_end - APP_START + 4):
        dev.set_record_raw(_forged_record(bad_len, image))
        assert dev.boot_decide() == 1, f"img_len {bad_len:#x} accepted"
    # exact-maximum img_len is VALID (inclusive bound) — protects <= from
    # regressing to <; img_crc32 is not checked in default builds
    dev.set_record_raw(_forged_record(dev.slot_capacity(0), image))
    assert dev.boot_decide() == 0
    dev.set_record_raw(expected_record(image))   # positive control: it was
    assert dev.boot_decide() == 0                # the geometry that gated


def test_record_geometry_gates_explicit_boot(dev):
    """Same forged record must fail the explicit-BOOT clean path."""
    image = bytes(range(1, 49))
    full_update(dev, image)
    dev.power_cycle()
    dev.set_record_raw(_forged_record(6, image))
    hello(dev)
    cmd_err(dev, CMD_BOOT, 1, bytes([0]), E_VERIFY, DET_NORECORD)


def test_ch57x_no_bless_after_write_then_rehello(dev57):
    """Re-HELLO must not reopen the XIP bless path: flash stays dirty for
    the whole power cycle, so a zero-write COMMIT cannot attest."""
    image = bytes(range(96))
    hello(dev57)
    erase(dev57, APP_START, BLOCK, seq=1)
    write(dev57, APP_START, image[:48], 2)
    hello(dev57, 3)                              # new session, same power
    cmd_err(dev57, CMD_COMMIT, 4, u32(48) + u32(zlib.crc32(image[:48])),
            E_VERIFY, DET_NONSEQ)
    assert dev57.record_raw() == dev57.erased(dev57.slot_record_addr(0), RECORD_SIZE)


def test_ch57x_no_bless_after_erase_only(dev57):
    """ERASE alone dirties XIP; a zero-write COMMIT must not bless stale
    pre-erase bytes into a record."""
    prior = bytes(range(10, 74))
    swd_install(dev57, prior, slot=0)
    _slot, base, _cap = write_target(dev57)
    erase(dev57, base, BLOCK, seq=1)
    cmd_err(dev57, CMD_COMMIT, 2, u32(len(prior)) + u32(zlib.crc32(prior)),
            E_VERIFY, DET_NONSEQ)


# --- A/B slot selection -------------------------------------------------

def test_consecutive_updates_alternate_slots(dev):
    """Every update targets the slot the device is NOT currently able to
    boot, so the previous image stays whole until the new one is committed.
    Both images are in flash at once — that is what makes the fallback in
    test_boot_mid_update_falls_back_to_the_untouched_slot real."""
    first = bytes(range(1, 65))
    second = bytes(range(100, 228))
    assert full_update(dev, first) == 0
    dev.power_cycle()
    assert dev.boot_select() == 0
    assert full_update(dev, second) == 1
    dev.power_cycle()
    assert dev.boot_select() == 1
    assert dev.flash_read(dev.slot_base(0), len(first)) == first
    assert dev.flash_read(dev.slot_base(1), len(second)) == second
    assert full_update(dev, first) == 0          # and back round to A
    dev.power_cycle()
    assert dev.boot_select() == 0


def test_hello_reports_the_slot_lifecycle(dev):
    """Nothing bootable, then A active and B the target, then the reverse."""
    active, wslot, base, cap = slot_view(hello(dev))
    assert (active, wslot, base) == (OB_SLOT_ID_NONE, 0, dev.slot_base(0))
    assert cap == dev.slot_capacity(0)
    full_update(dev, bytes(range(1, 65)))
    dev.power_cycle()
    assert slot_view(hello(dev))[:3] == (0, 1, dev.slot_base(1))
    full_update(dev, bytes(range(2, 66)))
    dev.power_cycle()
    assert slot_view(hello(dev))[:3] == (1, 0, dev.slot_base(0))


def test_the_active_slot_is_unreachable(dev):
    """The range gate is bounded by the write slot rather than by the app
    region, so no command a host can send names the image the device is
    currently able to boot — by mistake, by a stale address, or on purpose."""
    image = bytes(range(1, 65))
    slot = full_update(dev, image)
    dev.power_cycle()
    wslot, base, cap = write_target(dev)
    assert wslot != slot
    live = dev.slot_base(slot)
    cmd_err(dev, CMD_ERASE, 1, u32(live) + u32(BLOCK), E_ADDR, DET_RANGE)
    cmd_err(dev, CMD_WRITE, 2, u32(live) + bytes(4), E_ADDR, DET_RANGE)
    # The write slot's own record block is off limits too — only the disarm
    # step may erase it, which is why capacity stops one block short.
    cmd_err(dev, CMD_ERASE, 4, u32(base + cap) + u32(BLOCK), E_ADDR, DET_RANGE)
    # CRC is NOT confined this way: it changes nothing, and `openboot verify`
    # has to be able to read the committed image, which is never the target.
    pl, _act = cmd_ok(dev, CMD_CRC, 5, u32(live) + u32(len(image)))
    assert int.from_bytes(pl[1:5], "little") == zlib.crc32(image)
    assert dev.flash_read(live, len(image)) == image
    assert dev.violations() == 0


def test_commit_moves_the_write_target_within_one_session(dev):
    """COMMIT is the commit point, so it is where the target moves: without
    a new HELLO the slot just committed becomes unreachable and the other
    one takes the next update. That the second COMMIT attests at all is the
    session re-arm working — on CH57x the sequential run has to be
    re-anchored to the new base or nothing there can be attested."""
    first = bytes(range(1, 65))
    slot = full_update(dev, first)               # session stays open
    other = dev.slot_base(1 - slot)
    cmd_err(dev, CMD_ERASE, 0x61, u32(dev.slot_base(slot)) + u32(BLOCK),
            E_ADDR, DET_RANGE)
    second = bytes(range(100, 164))
    erase(dev, other, BLOCK, seq=0x62)
    seq = 0x63
    for off in range(0, len(second), MAX_WRITE):
        write(dev, other + off, second[off:off + MAX_WRITE], seq)
        seq += 1
    commit(dev, second, seq)
    assert dev.record_raw(1 - slot) == expected_record(second, generation=2)
    dev.power_cycle()
    assert dev.boot_select() == 1 - slot


# --- power-cut injection ------------------------------------------------

def _drive_update_frames(dev, image):
    """Fire a full update, ignoring responses (host oblivious to the cut)."""
    resp, _ = dev.frame(frame(CMD_HELLO, 0, HELLO_PAYLOAD))
    base = slot_view(parse(resp)[2])[2]
    blocks = (len(image) + BLOCK - 1) // BLOCK
    dev.frame(frame(CMD_ERASE, 1, u32(base) + u32(blocks * BLOCK)))
    seq = 2
    for off in range(0, len(image), MAX_WRITE):
        dev.frame(frame(CMD_WRITE, seq & 0xFF,
                        u32(base + off) + image[off:off + MAX_WRITE]))
        seq += 1
    dev.frame(frame(CMD_COMMIT, seq & 0xFF,
                    u32(len(image)) + u32(zlib.crc32(image))))
    return base


def test_power_cut_sweep(dev):
    rng = random.Random(0xCB7)
    image = bytes(rng.randrange(256) for _ in range(4096))
    dev.reset()
    _drive_update_frames(dev, image)     # clean run to count flash ops
    total = dev.op_total()
    dev.power_cycle()
    assert dev.boot_decide() == 0        # sanity: uncut update boots
    for n in range(total):
        dev.reset()
        dev.set_fail_after(n)
        _drive_update_frames(dev, image)
        dev.power_cycle()
        assert dev.boot_decide() == 1, f"cut at op {n} must stay in bootloader"
    dev.reset()
    dev.set_fail_after(total)            # cut lands after the last op
    _drive_update_frames(dev, image)
    dev.power_cycle()
    assert dev.boot_decide() == 0


def test_power_cut_disarm_ordering(dev):
    image_a = bytes(range(1, 129))
    slot = full_update(dev, image_a)
    dev.power_cycle()
    assert dev.boot_decide() == 0
    # fail_after=0 cuts at the record invalidate itself; fail_after=1 lets
    # the invalidate land and cuts the image erase that follows it. Under
    # A/B both are survivable for the same reason: the invalidate can only
    # ever reach the write slot's own record block, so wherever the cut
    # falls the previous image and its record are untouched.
    for fail_after in (0, 1):
        dev.power_cycle()
        dev.set_fail_after(fail_after)
        wslot, base, _cap = write_target(dev)
        assert wslot != slot
        cmd_err(dev, CMD_ERASE, 1, u32(base) + u32(BLOCK), E_FLASH)
        dev.power_cycle()
        assert dev.boot_decide() == 0            # the old image still boots
        assert dev.boot_select() == slot         # and it is the one chosen
        assert dev.flash_read(dev.slot_base(slot), len(image_a)) == image_a


# --- boot decision ------------------------------------------------------

def _setup_app(dev, record_ok, erased_first):
    dev.reset()
    if record_ok:
        image = (dev.erased(APP_START, 64) if erased_first
                 else bytes(range(1, 65)))
        full_update(dev, image)
    else:
        _slot, base, _cap = write_target(dev)
        erase(dev, base, BLOCK)
        if not erased_first:
            write(dev, base, bytes(range(1, 49)), 2)
    dev.power_cycle()


def test_boot_decision_truth_table(dev):
    for record_ok in (False, True):
        for bootreq in (False, True):
            for erased_first in (False, True):
                _setup_app(dev, record_ok, erased_first)
                if bootreq:
                    dev.set_bootreq(BOOTREQ_MAGIC)
                stay = bootreq or not record_ok or erased_first
                got = dev.boot_decide()
                assert got == (1 if stay else 0), \
                    f"record={record_ok} bootreq={bootreq} erased={erased_first}"
                if bootreq:
                    assert dev.get_bootreq() == 0         # consumed
    # non-magic bootreq value must not hold the device in the bootloader
    _setup_app(dev, True, False)
    dev.set_bootreq(0x12345678)
    assert dev.boot_decide() == 0
    assert dev.get_bootreq() == 0x12345678                # left alone


# --- retry/reset reconciliation regressions ------------------------------

def test_write_retry_idempotent_stream(dev):
    """A retried WRITE (device OK lost, host re-sends the same chunk) must
    not poison the stream CRC: COMMIT still attests the session on CH57x."""
    image = bytes(range(256)) * 3
    hello(dev)
    erase(dev, APP_START, BLOCK, 1)
    seq = 2
    for off in range(0, len(image), MAX_WRITE):
        chunk = image[off:off + MAX_WRITE]
        write(dev, APP_START + off, chunk, seq & 0xFF)
        seq += 1
        if off == MAX_WRITE:                 # retry the second chunk once
            write(dev, APP_START + off, chunk, seq & 0xFF)
            seq += 1
    commit(dev, image, seq & 0xFF)
    assert dev.flash_read(APP_START, len(image)) == image
    assert dev.record_raw() == expected_record(image)


def test_boot_stay_sets_bootreq_magic(dev):
    """BOOT mode 1 must arm the RAM boot-request word so the device comes
    back up in the bootloader even though a valid, bootable app exists."""
    image = bytes(range(64)) * 2
    full_update(dev, image)
    dev.set_bootreq(0)
    rcmd, pl, act = send(dev, CMD_BOOT, 7, bytes([1]))
    assert rcmd == 0x86 and pl[0] == OK and act == ACT_RESET
    assert dev.get_bootreq() == BOOTREQ_MAGIC
    assert dev.boot_decide() == 1            # consumed: stays in bootloader
    assert dev.get_bootreq() == 0
    dev.power_cycle()                        # XIP coherent again (F26 gone)
    assert dev.boot_decide() == 0            # next power-up runs the app


# --- wrong-variant image: the silicon clamps the app region ---------------

def test_hello_reports_the_silicon_app_end_not_the_build(dev):
    """A ch592 image on a CH591 must not advertise 448 KiB on a 192 KiB die.

    Nothing else catches this: the family id and app end in HELLO are
    build-time constants, so without the clamp the host would erase and
    write a range the part does not have.
    """
    smaller = APP_START + 0x1000
    dev.set_silicon_app_end(smaller)
    pl = hello(dev)
    assert int.from_bytes(pl[12:16], "little") == smaller


def test_writes_past_the_silicon_end_are_refused(dev):
    smaller = APP_START + BLOCK
    assert smaller < dev.app_end, "test needs the clamp to be the tighter bound"
    dev.set_silicon_app_end(smaller)
    _active, _wslot, base, cap = slot_view(hello(dev))
    # A slot the silicon cannot hold is unusable WHOLESALE rather than
    # shrunk: shrinking it would move the record and break the address the
    # application was linked against. So the clamp does not narrow the
    # writable window, it removes it — and HELLO says so before the host
    # tries anything.
    assert cap == 0
    cmd_err(dev, CMD_ERASE, 1, u32(base) + u32(BLOCK), E_ADDR, DET_RANGE)
    cmd_err(dev, CMD_WRITE, 2, u32(base) + bytes(4), E_ADDR, DET_RANGE)
    cmd_err(dev, CMD_COMMIT, 3, u32(4) + u32(0), E_ADDR, DET_RANGE)


def test_commit_length_is_bounded_by_the_silicon(dev):
    smaller = APP_START + BLOCK
    dev.set_silicon_app_end(smaller)
    hello(dev)
    over = smaller - APP_START + 4
    cmd_err(dev, CMD_COMMIT, 2, u32(over) + u32(0), E_ADDR, DET_RANGE)


def test_unknown_chip_id_falls_back_to_the_build(dev):
    """Refusing to run on an unrecognised id would strand a user on a new
    variant, so the compiled bound is kept and the host cross-checks."""
    dev.set_silicon_app_end(0)
    pl = hello(dev)
    assert int.from_bytes(pl[12:16], "little") == dev.app_end


# --- board-clamped build bound: the build is tighter than the silicon -----

def test_a_board_clamped_build_bound_holds_on_larger_silicon(dev):
    """The OpenDongle ch570 board sets APP_END below the silicon end so the
    dongle's bond page (0x3A000) and the boot record page (0x3B000) are
    outside OBP reach. min(silicon, build) must let the BUILD bound win —
    the mirror image of the silicon-clamp tests above."""
    dev.set_silicon_app_end(dev.app_end + 0x1000)
    pl = hello(dev)
    assert int.from_bytes(pl[12:16], "little") == dev.app_end
    _active, _wslot, base, cap = slot_view(pl)
    assert cap != 0, "the build bound must still leave slot A usable"
    # the last in-bound block of the slot still works...
    erase(dev, base + cap - BLOCK, BLOCK, seq=1)
    write(dev, base + cap - MAX_WRITE, bytes(MAX_WRITE), seq=2)
    # ...but nothing at or past the slot bound does, silicon or not
    cmd_err(dev, CMD_WRITE, 3, u32(base + cap) + bytes(4), E_ADDR, DET_RANGE)
    cmd_err(dev, CMD_ERASE, 4, u32(base + cap) + u32(BLOCK), E_ADDR, DET_RANGE)
    cmd_err(dev, CMD_COMMIT, 5, u32(cap + 4) + u32(0), E_ADDR, DET_RANGE)


# --- idle auto-boot timing ----------------------------------------------
# main.c needs a transport and is only syntax-checked, so the timeout
# arithmetic lives in boot_decision.c specifically to be reachable here.
# Before this, nothing in the suite covered idle auto-boot at all.

def test_idle_timeout_of_zero_never_elapses(dev):
    """0 disables idle auto-boot (PROTOCOL.md section 10). It must not
    degenerate into 'expire immediately', which would boot the app out from
    under a board that deliberately asked to wait forever."""
    assert not dev.idle_elapsed(0, 0, 0)
    assert not dev.idle_elapsed(0, 0xFFFFFFFF, 0)


def test_idle_elapses_exactly_at_the_deadline(dev):
    assert not dev.idle_elapsed(1000, 10999, 10000)
    assert dev.idle_elapsed(1000, 11000, 10000)
    assert dev.idle_elapsed(1000, 11001, 10000)


def test_idle_is_wrap_safe_across_the_tick_rollover(dev):
    """start near 2^32 and now past the wrap: unsigned subtraction has to
    give the true elapsed ticks, not a whole-counter-period answer."""
    start = 0xFFFFF000
    assert not dev.idle_elapsed(start, 0x00000FFF, 10000)   # 8191 ticks elapsed
    assert dev.idle_elapsed(start, 0x00001710, 10000)       # 10000 ticks elapsed


# --- A/B slot selection --------------------------------------------------
# The invariant from docs/AB-UPDATE.md: at every instant at least one slot
# holds a CRC-valid record describing a CRC-valid image, and the bootloader
# boots the highest valid generation.

SLOT_A, SLOT_B, SLOT_NONE = 0, 1, 0xFFFFFFFF


def place_slot(dev, slot, image, generation):
    """Put `image` and a matching record into `slot`, bypassing OBP."""
    dev.write_flash(dev.slot_base(slot), image)
    dev.set_record_raw(expected_record(image, generation), slot)


def test_slots_do_not_overlap_and_records_sit_in_their_own_slot(dev):
    a, b = dev.slot_base(SLOT_A), dev.slot_base(SLOT_B)

    assert a + dev.slot_capacity(SLOT_A) <= dev.slot_record_addr(SLOT_A) < b
    assert b + dev.slot_capacity(SLOT_B) <= dev.slot_record_addr(SLOT_B)
    assert dev.slot_record_addr(SLOT_B) + BLOCK <= dev.app_end


def test_higher_generation_wins(dev):
    older, newer = bytes(range(1, 65)), bytes(range(65, 129))
    place_slot(dev, SLOT_A, older, 4)
    place_slot(dev, SLOT_B, newer, 5)

    assert dev.boot_select() == SLOT_B

    # and the other way round, to prove it is the generation and not the order
    place_slot(dev, SLOT_A, older, 9)
    assert dev.boot_select() == SLOT_A


def test_a_newer_record_with_a_broken_image_falls_back(dev):
    """The interrupted-update case: slot B's record claims a newer generation
    but its image never landed, so the older slot must still boot."""
    good = bytes(range(1, 65))
    place_slot(dev, SLOT_A, good, 1)
    # record says 64 bytes are there; the slot is left erased
    dev.set_record_raw(expected_record(good, 2), SLOT_B)

    assert dev.boot_select() == SLOT_A
    assert dev.highest_generation() == 2


def test_commit_outranks_a_newer_record_with_a_broken_image(dev):
    """Selection may fall back to an older image, but COMMIT must still
    outrank every valid record generation observed before mutation."""
    old = bytes(range(1, 65))
    new = bytes(range(65, 129))
    place_slot(dev, SLOT_A, old, 4)
    dev.set_record_raw(expected_record(new, 9), SLOT_B)  # image stays erased

    dev.power_cycle()
    assert full_update(dev, new) == SLOT_B
    assert dev.record_raw(SLOT_B) == expected_record(new, generation=10)


def test_no_valid_slot_selects_none(dev):
    assert dev.boot_select() == SLOT_NONE


def test_boot_decision_jumps_to_the_selected_slot(dev):
    image = bytes(range(1, 65))
    place_slot(dev, SLOT_B, image, 3)

    assert dev.boot_decide() == 0, "should have jumped"
    assert dev.jumped_to() == dev.slot_base(SLOT_B)


def test_a_corrupt_record_never_validates_its_slot(dev):
    image = bytes(range(1, 65))
    place_slot(dev, SLOT_A, image, 1)
    assert dev.boot_select() == SLOT_A

    good = expected_record(image, 1)
    body = good[:RECORD_SIZE - 4]
    cap = dev.slot_capacity(SLOT_A)

    def resealed(mutated):
        """Re-seal rec_crc32 over a mutated body. Without this every case
        fails on the CRC check — which runs first — and the checks each case
        is named for are never reached."""
        assert len(mutated) == RECORD_SIZE - 4
        return mutated + u32(zlib.crc32(mutated))

    cases = {
        "bad magic": resealed(u32(RECORD_MAGIC ^ 1) + body[4:]),
        "generation 0": resealed(body[:4] + u32(0) + body[8:]),
        "reserved byte nonzero": resealed(body[:16] + b"\x01" + body[17:]),
        "img_len 0": resealed(body[:8] + u32(0) + body[12:]),
        "img_len misaligned": resealed(body[:8] + u32(len(image) + 1) + body[12:]),
        "img_len past capacity": resealed(body[:8] + u32(cap + 4) + body[12:]),
        # The one case that must NOT be resealed: it IS the CRC check.
        "broken rec_crc32": good[:-1] + b"\xAA",
    }
    for name, bad in cases.items():
        assert len(bad) == RECORD_SIZE
        dev.set_record_raw(bad, SLOT_A)
        assert dev.boot_select() == SLOT_NONE, f"accepted a record with {name}"


def test_selection_reports_the_highest_valid_record_generation(dev):
    assert dev.highest_generation() == 0

    place_slot(dev, SLOT_A, bytes(range(1, 65)), 7)
    assert dev.highest_generation() == 7

    place_slot(dev, SLOT_B, bytes(range(1, 65)), 11)
    assert dev.highest_generation() == 11


# --- the simulator's flash model itself ----------------------------------
# These guard the HARNESS, not the firmware. A model that lets a write set
# bits, or that never tears a write, silently hides the exact defects the A/B
# invariant is built to survive.

def test_programming_can_only_clear_bits(dev):
    """Writing over already-programmed bytes ANDs, as NOR flash does, so the
    verify inside the write path must reject it."""
    hello(dev)
    erase(dev, APP_START, BLOCK)
    write(dev, APP_START, bytes([0xF0] * 8), 2)
    assert dev.flash_read(APP_START, 8) == bytes([0xF0] * 8)

    # 0x0F needs bits 0-3 set, which programming cannot do
    cmd_err(dev, CMD_WRITE, 3, u32(APP_START) + bytes([0x0F] * 8), E_FLASH)
    assert dev.flash_read(APP_START, 8) == bytes([0xF0 & 0x0F] * 8)


def test_a_cut_write_leaves_a_partial_prefix(dev):
    """Row three of the invariant table depends on a torn write being
    possible: a half-written record must fail its CRC, not vanish."""
    hello(dev)
    erase(dev, APP_START, BLOCK)
    dev.set_fail_after(0)                    # next mutating op is cut

    send(dev, CMD_WRITE, 2, u32(APP_START) + bytes([0xAA] * 16))
    got = dev.flash_read(APP_START, 16)

    assert got[:8] == bytes([0xAA] * 8), "no prefix landed; write was atomic"
    assert got[8:] != bytes([0xAA] * 8), "whole write landed despite the cut"


def test_the_last_usable_generation_is_the_ceiling_minus_one(dev):
    """Generations 1..0xFFFFFFFE are storable; a commit that would need
    0xFFFFFFFF is refused before any flash is touched.

    Storing the ceiling would be a silent brick of the update path:
    A saturated generation cannot be outranked, so the OTHER slot would
    otherwise store the same value and ob_boot_select() would resolve the tie by slot
    position — the new image committed, attested, and never booted. The
    cached-generation increment could also wrap the ceiling to 0, a record
    ob_record_load() rejects. Unreachable by wear (~2^32 commits against
    ~10^4..10^5 erase cycles); reachable by a hand-crafted record, which is
    what these plant."""
    image = bytes(range(1, 65))

    # One below the danger zone: the next generation is 0xFFFFFFFE, storable.
    dev.reset()
    place_slot(dev, SLOT_A, image, 0xFFFFFFFD)
    dev.power_cycle()
    slot, base, _cap = write_target(dev)
    assert slot == SLOT_B
    dev.write_flash(base, image)                  # stage the write slot (bless)
    commit(dev, image, 1)
    assert dev.record_raw(slot) == expected_record(image, generation=0xFFFFFFFE)

    # At the edge: the next generation would be the ceiling. Refused, with
    # nothing written — the write slot's record block still reads erased.
    dev.reset()
    place_slot(dev, SLOT_A, image, 0xFFFFFFFE)
    dev.power_cycle()
    slot, base, _cap = write_target(dev)
    assert slot == SLOT_B
    dev.write_flash(base, image)
    before = dev.op_total()
    cmd_err(dev, CMD_COMMIT, 1, u32(len(image)) + u32(zlib.crc32(image)),
            E_FLASH, 0)
    assert dev.op_total() == before, "the refused COMMIT touched flash"
    assert dev.record_raw(slot) == dev.erased(dev.slot_record_addr(slot),
                                              RECORD_SIZE)


# --- factory images -----------------------------------------------------
# compose_factory.py builds the boot record offline, with no device and no
# host tool. Nothing else proves the bytes it writes are bytes the bootloader
# accepts, so these drive the composed image through the REAL boot decision.

def test_a_blessed_factory_image_boots_with_no_host_involved(dev):
    """The factory case: one blob written to a blank part, and it comes up
    running the application — no `openboot bless`, no host tool, no session."""
    import compose_factory as cf

    dev.reset()
    boot = bytes((i * 3 + 1) & 0xFF for i in range(2048))   # stand-in OpenBoot
    app = bytes((i * 7) & 0xFF for i in range(1000))
    cap = dev.slot_capacity(SLOT_A)

    image = cf.compose_factory(boot, app, bless_capacity=cap)
    assert len(image) == dev.slot_record_addr(SLOT_A) + RECORD_SIZE, \
        "the record must land exactly where the bootloader reads it"
    dev.write_flash(0, image)

    assert dev.boot_select() == SLOT_A
    assert dev.boot_decide() == 0                    # jumped rather than stayed
    assert dev.jumped_to() == dev.slot_base(SLOT_A)
    assert dev.record_raw(SLOT_A) == expected_record(app, generation=1)


def test_an_unblessed_factory_image_stays_in_the_bootloader(dev):
    """The other half of the contract: omit the record and the part is NOT
    bootable, which is the behaviour a bring-up image wants and a production
    one does not."""
    import compose_factory as cf

    dev.reset()
    boot = bytes((i * 3 + 1) & 0xFF for i in range(2048))
    app = bytes((i * 7) & 0xFF for i in range(1000))
    dev.write_flash(0, cf.compose_factory(boot, app))
    assert dev.boot_select() == SLOT_NONE
    assert dev.boot_decide() == 1                    # stays


def test_a_factory_image_is_updated_into_the_other_slot(dev):
    """A factory part is an ordinary A/B device afterwards: its first update
    goes to slot B and outranks the factory record."""
    import compose_factory as cf

    dev.reset()
    app = bytes((i * 7) & 0xFF for i in range(1000))
    cap = dev.slot_capacity(SLOT_A)
    dev.write_flash(0, cf.compose_factory(bytes(2048), app, bless_capacity=cap))
    assert dev.boot_select() == SLOT_A

    dev.power_cycle()
    assert full_update(dev, bytes(range(1, 65))) == SLOT_B
    dev.power_cycle()
    assert dev.boot_select() == SLOT_B
    ra, rb = dev.record_raw(SLOT_A), dev.record_raw(SLOT_B)
    assert int.from_bytes(rb[4:8], "little") > int.from_bytes(ra[4:8], "little")


def test_a_failed_disarm_leaves_app_flash_alone(dev):
    """Disarm before mutation: if invalidating the record fails, the erase
    must not be attempted at all.

    Op COUNT is the only thing that separates the two behaviours here. The
    status is E_FLASH either way and the flash is unchanged either way,
    because the simulator keeps failing once it starts - so removing the guard
    changed nothing any assertion could see. On real silicon a TRANSIENT
    invalidate failure would let the erase run with a valid record still in
    place, which is precisely what this ordering exists to prevent."""
    _slot, base, _cap = write_target(dev)
    dev.set_fail_after(0)                       # the record invalidate fails
    before = dev.op_total()
    cmd_err(dev, CMD_ERASE, 1, u32(base) + u32(BLOCK), E_FLASH)
    assert dev.op_total() - before == 1, "the erase was attempted after a failed disarm"


def test_a_runt_frame_is_ignored(dev):
    """Shorter than the 8-byte overhead: there is no header to trust, so the
    device must not answer at all. Nothing covered this before.

    It pins the CONTRACT, not a particular line: two length checks enforce it
    and either alone suffices, so this only fails when both are gone. The
    first one's own contribution - not reading header bytes that were never
    received - cannot be observed here, because the harness always hands the
    core a full-size buffer."""
    for length in range(0, 8):
        resp, act = dev.frame(bytes(length))
        assert resp == b"", f"answered a {length}-byte frame"
        assert act == ACT_NONE


# --- OB_BOOT_IMAGE_CRC --------------------------------------------------
# The opt-in boot-time image check. It decides whether a device boots, and
# until now it was only ever syntax-checked: nothing executed the comparison,
# and deleting it from boot_decision.c broke no test. These run the same core
# built both ways and require them to DISAGREE, which is the only way to show
# the check is what made the difference.

@pytest.fixture()
def dev57crc():
    d = get_device("ch57x_imagecrc")
    d.reset()
    return d


def _install(dev, image, generation=1):
    """Put an image and a matching record in slot A out of band, as SWD or a
    factory programmer would."""
    dev.reset()
    dev.write_flash(dev.slot_base(SLOT_A), image)
    dev.set_record_raw(expected_record(image, generation), SLOT_A)


def test_image_crc_build_boots_an_image_that_matches_its_record(dev57crc):
    image = bytes(range(1, 65))
    _install(dev57crc, image)
    assert dev57crc.boot_select() == SLOT_A
    assert dev57crc.boot_decide() == 0
    assert dev57crc.jumped_to() == dev57crc.slot_base(SLOT_A)


def test_image_crc_build_refuses_an_image_that_does_not(dev57crc):
    """A byte changed under a surviving record — corruption, or an
    out-of-band reflash that diverges from what was committed."""
    image = bytes(range(1, 65))
    _install(dev57crc, image)
    corrupt = bytes([image[0] ^ 0x01]) + image[1:]
    dev57crc.write_flash(dev57crc.slot_base(SLOT_A), corrupt)
    assert dev57crc.boot_select() == SLOT_NONE
    assert dev57crc.boot_decide() == 1, "must stay in the bootloader"


def test_without_the_option_the_same_corruption_still_boots(dev57):
    """The contrast that makes the test above meaningful. The default build
    checks the record and the first word only, so it boots the corrupted
    image — which is exactly what the option exists to change."""
    image = bytes(range(1, 65))
    _install(dev57, image)
    dev57.write_flash(dev57.slot_base(SLOT_A), bytes([image[0] ^ 0x01]) + image[1:])
    assert dev57.boot_select() == SLOT_A
    assert dev57.boot_decide() == 0


def test_image_crc_checks_the_recorded_length_only(dev57crc):
    """Documented prefix property: bytes past img_len are never checked, so a
    longer image sharing the recorded prefix still validates."""
    image = bytes(range(1, 65))
    _install(dev57crc, image)
    dev57crc.write_flash(dev57crc.slot_base(SLOT_A) + len(image), b"\xA5" * 32)
    assert dev57crc.boot_select() == SLOT_A
