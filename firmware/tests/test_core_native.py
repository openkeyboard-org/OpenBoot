"""Host-native tests for the OpenBoot portable core (real C sources compiled
against the simulated-flash mock port; see core_host/)."""
import random
import zlib

import pytest

from ob_native import (
    ACT_NONE, ACT_RESET, APP_START, BLOCK, BOOTREQ_MAGIC,
    CMD_BOOT, CMD_COMMIT, CMD_CRC, CMD_ERASE, CMD_HELLO, CMD_READ, CMD_WRITE,
    DET_ALIGN, DET_MISMATCH, DET_NONSEQ, DET_NORECORD, DET_RANGE,
    E_ADDR, E_ARG, E_CMD, E_FLASH, E_LEN, E_NOT_ERASED, E_PROTO, E_STATE,
    E_VERIFY, MAX_WRITE, OK, RECORD_MAGIC, RECORD_SIZE, RSVD,
    frame, get_device, load_golden,
)

GOLDEN = load_golden()
HELLO_PAYLOAD = b"OBP1" + bytes([0, 1])


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


def full_update(dev, image, whole_app=False):
    """HELLO + ERASE + sequential WRITE + COMMIT. Caller resets first."""
    hello(dev)
    seq = 1
    if whole_app:
        a = APP_START
        while a < dev.app_end:
            length = min(0x8000, dev.app_end - a)   # host chunks <= 32 KiB
            erase(dev, a, length, seq & 0xFF)
            seq += 1
            a += length
    else:
        blocks = (len(image) + BLOCK - 1) // BLOCK
        erase(dev, APP_START, blocks * BLOCK, seq & 0xFF)
        seq += 1
    for off in range(0, len(image), MAX_WRITE):
        write(dev, APP_START + off, image[off:off + MAX_WRITE], seq & 0xFF)
        seq += 1
    commit(dev, image, seq & 0xFF)


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
    assert len(pl) == 36
    assert pl[0] == OK
    assert (pl[1], pl[2]) == (0, 1)                       # proto 0.1
    assert pl[3] == 9                                     # chip_rev
    assert int.from_bytes(pl[4:6], "little") == 0x000A    # bl_version v0.10
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
    full_update(dev, image, whole_app=True)
    assert dev.flash_read(APP_START, len(image)) == image
    assert dev.record_raw() == expected_record(image)
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
    cmd_err(dev, CMD_HELLO, 1, b"OBP1" + bytes([2, 0]), E_PROTO, 0)
    # pre-1.0 (major 0): the minor must match exactly too
    cmd_err(dev, CMD_HELLO, 2, b"OBP1" + bytes([0, 2]), E_PROTO, 0)
    cmd_err(dev, CMD_HELLO, 3, b"OBPX" + bytes([0, 1]), E_ARG, 0)
    # none of them opened a session
    cmd_err(dev, CMD_ERASE, 4, u32(APP_START) + u32(BLOCK), E_STATE, 0)


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


def test_ch57x_nonseq_commit_rejected(dev57):
    image = bytes(range(16, 80))
    hello(dev57)
    erase(dev57, APP_START, BLOCK)
    write(dev57, APP_START + 48, image[48:], 2)           # out of order
    write(dev57, APP_START, image[:48], 3)
    cmd_err(dev57, CMD_COMMIT, 4, u32(len(image)) + u32(zlib.crc32(image)),
            E_VERIFY, DET_NONSEQ)
    assert dev57.record_raw() == dev57.erased(dev57.slot_record_addr(0), RECORD_SIZE)


def test_ch57x_short_stream_commit_rejected(dev57):
    """Sequential but shorter than img_len: also NONSEQ."""
    hello(dev57)
    erase(dev57, APP_START, BLOCK)
    write(dev57, APP_START, bytes(48), 2)
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
    full_update(dev57, image)
    dev57.power_cycle()                  # image now "SWD-flashed" history
    hello(dev57)
    _pl, act = commit(dev57, image, 1)   # write_count == 0 -> XIP path
    assert act == ACT_NONE
    # generation 2: nothing was mutated, so the gen-1 record from the earlier
    # update survived into this session and the new one has to outrank it.
    assert dev57.record_raw() == expected_record(image, generation=2)
    assert dev57.violations() == 0


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
    full_update(dev, image)
    dev.power_cycle()                   # coherent out-of-band image history
    hello(dev)
    commit(dev, image, 1)
    after_bless = dev.op_total()

    commit(dev, image, 2)
    assert dev.op_total() == after_bless
    hello(dev, 3)
    commit(dev, image, 4)
    assert dev.op_total() == after_bless


def test_commit_replay_requires_exact_tuple(dev):
    image = bytes(range(1, 97))
    full_update(dev, image)
    before = dev.op_total()
    cmd_err(dev, CMD_COMMIT, 0x40,
            u32(len(image)) + u32(zlib.crc32(image) ^ 1),
            E_VERIFY, DET_MISMATCH)
    assert dev.op_total() == before


def test_mutation_invalidates_commit_replay(dev):
    image = bytes(range(1, 97))
    full_update(dev, image)
    erase(dev, APP_START, BLOCK, seq=0x40)
    before = dev.op_total()
    cmd_err(dev, CMD_COMMIT, 0x41,
            u32(len(image)) + u32(zlib.crc32(image)), E_VERIFY)
    assert dev.op_total() == before


def test_power_cycle_forgets_commit_replay_cache(dev):
    image = bytes(range(1, 97))
    full_update(dev, image)
    dev.power_cycle()
    hello(dev)
    before = dev.op_total()
    commit(dev, image, 1)
    # Two mutating ops, not one: storing a record erases its block before
    # writing it, because flash only clears bits.
    assert dev.op_total() == before + 2


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

def test_boot_rejected_between_invalidate_and_commit(dev):
    """From the first mutation until a successful COMMIT, BOOT must refuse
    to trust the record — on CH57x the stale (F26) view still reads as the
    old, valid record after the invalidate."""
    image = bytes(range(64))
    full_update(dev, image)
    dev.power_cycle()
    hello(dev)
    erase(dev, APP_START, BLOCK, seq=1)          # invalidates the record
    cmd_err(dev, CMD_BOOT, 2, bytes([0]), E_VERIFY, DET_NORECORD)


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
    full_update(dev57, image)
    dev57.power_cycle()                          # image is now history
    hello(dev57)
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
    full_update(dev57, prior)
    dev57.power_cycle()                          # committed history
    hello(dev57)
    erase(dev57, APP_START, BLOCK, seq=1)
    cmd_err(dev57, CMD_COMMIT, 2, u32(len(prior)) + u32(zlib.crc32(prior)),
            E_VERIFY, DET_NONSEQ)


# --- power-cut injection ------------------------------------------------

def _drive_update_frames(dev, image):
    """Fire a full update, ignoring responses (host oblivious to the cut)."""
    dev.frame(frame(CMD_HELLO, 0, HELLO_PAYLOAD))
    blocks = (len(image) + BLOCK - 1) // BLOCK
    dev.frame(frame(CMD_ERASE, 1, u32(APP_START) + u32(blocks * BLOCK)))
    seq = 2
    for off in range(0, len(image), MAX_WRITE):
        dev.frame(frame(CMD_WRITE, seq & 0xFF,
                        u32(APP_START + off) + image[off:off + MAX_WRITE]))
        seq += 1
    dev.frame(frame(CMD_COMMIT, seq & 0xFF,
                    u32(len(image)) + u32(zlib.crc32(image))))


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
    full_update(dev, image_a)
    dev.power_cycle()
    assert dev.boot_decide() == 0
    # Cut at the record invalidate itself: old record + old app intact.
    dev.power_cycle()
    dev.set_fail_after(0)
    hello(dev)
    cmd_err(dev, CMD_ERASE, 1, u32(APP_START) + u32(BLOCK), E_FLASH)
    dev.power_cycle()
    assert dev.boot_decide() == 0        # boots the old, untouched image
    assert dev.flash_read(APP_START, len(image_a)) == image_a
    # Cut just after the invalidate: no erase happened, but we must stay.
    dev.power_cycle()
    dev.set_fail_after(1)
    hello(dev)
    cmd_err(dev, CMD_ERASE, 1, u32(APP_START) + u32(BLOCK), E_FLASH)
    dev.power_cycle()
    assert dev.boot_decide() == 1        # disarmed before any mutation


# --- boot decision ------------------------------------------------------

def _setup_app(dev, record_ok, erased_first):
    dev.reset()
    if record_ok:
        image = (dev.erased(APP_START, 64) if erased_first
                 else bytes(range(1, 65)))
        full_update(dev, image)
    else:
        hello(dev)
        erase(dev, APP_START, BLOCK)
        if not erased_first:
            write(dev, APP_START, bytes(range(1, 49)), 2)
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


def test_bootpin_overrides_all(dev):
    _setup_app(dev, True, False)
    dev.set_bootpin(1)
    assert dev.boot_decide() == 1
    dev.set_bootpin(0)
    assert dev.boot_decide() == 0


def test_bootpin_consumes_a_pending_bootreq(dev):
    """A strap-held reset must still consume the app's one-shot request.

    Otherwise the magic survives in RAM, and the next BOOT — which now
    always answers OK and resets — finds it and stays in the bootloader
    instead of launching, so a successful BOOT silently fails to boot.
    """
    _setup_app(dev, True, False)
    dev.set_bootreq(BOOTREQ_MAGIC)
    dev.set_bootpin(1)
    assert dev.boot_decide() == 1                  # strap wins, as before
    assert dev.get_bootreq() == 0                  # ...and the request is spent
    dev.set_bootpin(0)
    assert dev.boot_decide() == 0                  # next reset launches the app


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
    dev.set_silicon_app_end(smaller)
    hello(dev)
    erase(dev, APP_START, BLOCK)
    # last legal chunk still works
    write(dev, smaller - MAX_WRITE, bytes(MAX_WRITE), seq=3)
    # one past the clamped end does not, even though the BUILD allows it
    assert smaller < dev.app_end, "test needs the clamp to be the tighter bound"
    cmd_err(dev, CMD_WRITE, 4, u32(smaller) + bytes(4), E_ADDR, DET_RANGE)
    cmd_err(dev, CMD_ERASE, 5, u32(smaller) + u32(BLOCK), E_ADDR, DET_RANGE)


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
    # the last in-bound block still works...
    erase(dev, dev.app_end - BLOCK, BLOCK, seq=1)
    write(dev, dev.app_end - MAX_WRITE, bytes(MAX_WRITE), seq=2)
    # ...but nothing at or past the build bound does, silicon or not
    cmd_err(dev, CMD_WRITE, 3, u32(dev.app_end) + bytes(4), E_ADDR, DET_RANGE)
    cmd_err(dev, CMD_ERASE, 4, u32(dev.app_end) + u32(BLOCK), E_ADDR, DET_RANGE)
    over = dev.app_end - APP_START + 4
    cmd_err(dev, CMD_COMMIT, 5, u32(over) + u32(0), E_ADDR, DET_RANGE)


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


def test_idle_is_wrap_safe_across_the_millisecond_rollover(dev):
    """start near 2^32 and now past the wrap: unsigned subtraction has to
    give the true elapsed time, not a ~49-day answer."""
    start = 0xFFFFF000
    assert not dev.idle_elapsed(start, 0x00000FFF, 10000)   # 8191 ms elapsed
    assert dev.idle_elapsed(start, 0x00001710, 10000)       # 10000 ms elapsed


@pytest.mark.parametrize("ticks_per_ms", [6400, 60000, 100000])
def test_ms_accumulate_loses_no_ticks_over_many_calls(dev, ticks_per_ms):
    """Sub-millisecond remainders must carry across calls: the main loop
    polls every few tens of microseconds, so most calls add no whole
    millisecond at all and a truncating implementation would never advance."""
    ms = rem = 0
    delta = ticks_per_ms // 10 + 7          # ~0.1 ms, deliberately not a divisor
    for _ in range(10000):
        ms, rem = dev.ms_accumulate(ms, rem, delta, ticks_per_ms)

    assert ms == (10000 * delta) // ticks_per_ms
    assert rem == (10000 * delta) % ticks_per_ms


@pytest.mark.parametrize("ticks_per_ms", [6400, 60000, 100000])
def test_ms_accumulate_survives_a_huge_delta(dev, ticks_per_ms):
    """A delta near 2^32 is what a missed counter wrap looks like. The
    arithmetic must not overflow — adding the delta into the remainder
    before dividing would."""
    ms, rem = dev.ms_accumulate(0, ticks_per_ms - 1, 0xFFFFFFFF, ticks_per_ms)

    assert ms == (0xFFFFFFFF + ticks_per_ms - 1) // ticks_per_ms
    assert rem == (0xFFFFFFFF + ticks_per_ms - 1) % ticks_per_ms


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

    for bad in (
        expected_record(image, 1)[:4] + b"\x00" + expected_record(image, 1)[5:],
        expected_record(image, 1)[:-1] + b"\xAA",              # broken rec_crc32
        expected_record(image, 1)[:16] + b"\x01" + expected_record(image, 1)[17:],
    ):
        dev.set_record_raw(bad, SLOT_A)
        assert dev.boot_select() == SLOT_NONE, f"accepted {bad[:8].hex()}"


def test_next_generation_outranks_every_valid_record(dev):
    assert dev.next_generation() == 1

    place_slot(dev, SLOT_A, bytes(range(1, 65)), 7)
    assert dev.next_generation() == 8

    place_slot(dev, SLOT_B, bytes(range(1, 65)), 11)
    assert dev.next_generation() == 12


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
