"""Unit tests for the open flash driver's sequencing (ports/flash_ch5xx.c).

The driver is compiled for the host against a recording register mock
(tests/flash_host/), one library per family because the archives they
reproduce diverge: gate bits, resume send count, begin-NOP count and the
info-window address OR. Every test replays the mock's event log through a
little transaction parser that mirrors the controller framing (CTRL=0,
CTRL=5, NOPs, opcode byte, payload, CTRL=0), so assertions read like the
SPI-NOR protocol the disassembled archives implement: WREN placement, address
byte order, dummy clocks, one page program per 256-byte page, four CTRL=0x15
pulses per word, wait-status polling, and gate open/close on every exit path.
"""
import ctypes
import importlib.util
from pathlib import Path

import pytest

_build_py = Path(__file__).resolve().parent / "core_host" / "build.py"
_spec = importlib.util.spec_from_file_location("ob_host_build", _build_py)
host_build = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(host_build)

OUT = Path(host_build.OUT)

# Event kinds, mirroring the enum in flash_host/ob_flash_mock.c.
CTRL_WR, DATA8_WR, DATA32_WR, GLOB_WR = 1, 2, 3, 4
CTRL_RD, DATA8_RD, DATA32_RD, GLOB_RD = 5, 6, 7, 8
NOP, SAFE_ON, SAFE_OFF = 9, 10, 11

# Driver error codes (flash_ch5xx.h).
E_ERASE_PARAM, E_ERASE_TIMEOUT = 0x51, 0x52
E_WRITE_PARAM, E_WRITE_TIMEOUT = 0x61, 0x62
E_VERIFY_PARAM, E_VERIFY_MISMATCH = 0x71, 0x72

WAIT_ITERS = 100        # OB_CPU_HZ(4000) / 40 in ob_flash_host_port.h

FAMILY = {
    # gate_write, gate_read, begin_nops, resume double-send, info-window OR
    "ch57x": dict(gw=0xE0, gr=0xE0, nops=2, resume2=True, info_or=0x00000),
    "ch59x": dict(gw=0xE0, gr=0x20, nops=1, resume2=False, info_or=0x80000),
}


class Drv:
    def __init__(self, fam: str, so: str = None):
        host_build.ensure_built()
        self.lib = ctypes.CDLL(str(OUT / f"ob_flash_{so or fam}.so"))
        self.lib.ob_ch5xx_flash_erase.restype = ctypes.c_uint32
        self.lib.ob_ch5xx_flash_write.restype = ctypes.c_uint32
        self.lib.ob_ch5xx_flash_verify.restype = ctypes.c_uint32
        self.fam = FAMILY[fam]

    def reset(self, glob=0x00, rd8_default=0x00):
        self.lib.ob_flmock_reset(ctypes.c_uint8(glob),
                                 ctypes.c_uint8(rd8_default))

    def push8(self, *vals):
        for v in vals:
            self.lib.ob_flmock_push_rd8(ctypes.c_uint8(v))

    def events(self):
        n = ctypes.c_uint32.in_dll(self.lib, "ob_flmock_ev_count").value
        lost = ctypes.c_uint32.in_dll(self.lib, "ob_flmock_ev_lost").value
        assert lost == 0, "mock event log overflowed"
        # The mock models the byte engine's busy bit: any synchronized access
        # without a preceding poll (a deleted ob_fl_busy) is a violation, in
        # EVERY test, not just a bespoke one.
        polls = ctypes.c_uint32.in_dll(self.lib, "ob_flmock_poll_violations")
        assert polls.value == 0, f"{polls.value} unpolled engine accesses"
        kinds = (ctypes.c_uint8 * n).in_dll(self.lib, "ob_flmock_ev_kind")
        vals = (ctypes.c_uint32 * n).in_dll(self.lib, "ob_flmock_ev_val")
        return list(zip(kinds, vals))

    def erase(self, addr, length):
        return self.lib.ob_ch5xx_flash_erase(addr, length)

    def write(self, addr, data: bytes):
        buf = (ctypes.c_uint8 * len(data)).from_buffer_copy(data)
        return self.lib.ob_ch5xx_flash_write(addr, buf, len(data))

    def verify(self, addr, data: bytes):
        buf = (ctypes.c_uint8 * len(data)).from_buffer_copy(data)
        return self.lib.ob_ch5xx_flash_verify(addr, buf, len(data))

    def uid(self):
        buf = (ctypes.c_uint32 * 4)(0xAAAAAAAA, 0xBBBBBBBB,
                                    0xCCCCCCCC, 0xDDDDDDDD)
        self.lib.ob_ch5xx_flash_uid_read(buf)
        return bytes(buf)

    def rom_info(self, addr):
        buf = (ctypes.c_uint32 * 4)(0x11111111, 0x22222222,
                                    0x33333333, 0x44444444)
        self.lib.ob_ch5xx_flash_rom_info_read(addr, buf)
        return bytes(buf)


def transactions(evs):
    """Group events into controller transactions: CTRL=0, CTRL=5, NOPs,
    opcode byte, payload until the CTRL=0 that ends the transaction. A
    transaction still open at the end of the log is a framing bug (a missing
    end()), not something to silently accept."""
    txs, i = [], 0
    while i < len(evs):
        if (evs[i] == (CTRL_WR, 0) and i + 1 < len(evs)
                and evs[i + 1] == (CTRL_WR, 5)):
            j = i + 2
            nops = 0
            while j < len(evs) and evs[j][0] == NOP:
                nops += 1
                j += 1
            assert j < len(evs), "log ends inside a transaction preamble"
            assert evs[j][0] == DATA8_WR, "transaction without an opcode"
            tx = dict(op=evs[j][1], nops=nops, out=[], inn=[],
                      w32=[], r32=[], pulses=0, ctrl_rds=0)
            j += 1
            closed = False
            while j < len(evs):
                if evs[j] == (CTRL_WR, 0):
                    closed = True
                    break
                k, v = evs[j]
                if k == DATA8_WR:
                    tx["out"].append(v)
                elif k == DATA8_RD:
                    tx["inn"].append(v)
                elif k == DATA32_WR:
                    tx["w32"].append(v)
                elif k == DATA32_RD:
                    tx["r32"].append(v)
                elif k == CTRL_RD:
                    tx["ctrl_rds"] += 1
                elif k == CTRL_WR and v == 0x15:
                    tx["pulses"] += 1
                j += 1
            assert closed, f"transaction op={tx['op']:#x} never ended"
            txs.append(tx)
            i = j
        else:
            i += 1
    return txs


def glob_writes(evs):
    return [v for k, v in evs if k == GLOB_WR]


def addr_bytes(a):
    return [(a >> 16) & 0xFF, (a >> 8) & 0xFF, a & 0xFF]


@pytest.fixture(params=["ch57x", "ch59x"])
def drv(request):
    d = Drv(request.param)
    d.reset()
    return d


# ---- gate open/close ------------------------------------------------------

def test_erase_opens_write_gate_and_closes_to_code_ofs(drv):
    drv.reset(glob=0x10)            # CODE_OFS set, like an offset-boot part
    assert drv.erase(0x2000, 0x1000) == 0
    evs = drv.events()
    gw = glob_writes(evs)
    assert gw[0] == 0x10 | drv.fam["gw"]
    assert gw[-1] == 0x10           # close keeps ONLY RB_ROM_CODE_OFS
    # Safe-access brackets every GLOB write.
    idx = [i for i, e in enumerate(evs) if e[0] == GLOB_WR]
    for i in idx:
        assert evs[i - 1][0] == SAFE_ON and evs[i + 1][0] == SAFE_OFF


def test_verify_gate_is_family_specific(drv):
    assert drv.verify(0x2000, b"\x00" * 4) == 0
    assert glob_writes(drv.events())[0] == drv.fam["gr"]


# ---- resume + begin framing ------------------------------------------------

def test_resume_and_begin_nops_match_the_family_archive(drv):
    assert drv.erase(0x2000, 0x1000) == 0
    evs = drv.events()
    txs = transactions(evs)
    resume = txs[0]
    assert resume["op"] == 0xFF
    assert resume["nops"] == drv.fam["nops"]
    # ISP572's FLASH_ROM_BEG_FF sends the byte twice with a busy poll
    # between; ISP583 once. (The poll itself is enforced globally by the
    # mock's engine model; ctrl_rds >= 1 pins that it happened here.)
    if drv.fam["resume2"]:
        assert resume["out"] == [0xFF] and resume["ctrl_rds"] >= 1
    else:
        assert resume["out"] == []


def test_open_prefix_is_gate_then_controller_close_then_resume(drv):
    """The archives' open sequence is strict: safe-access GLOB write, then
    CTRL=4 (controller close), then the 0xFF resume transaction. Raw-event
    assertion because CTRL=4 sits outside any parsed transaction."""
    assert drv.erase(0x2000, 0x1000) == 0
    evs = drv.events()
    g = next(i for i, e in enumerate(evs) if e[0] == GLOB_WR)
    assert evs[g - 1] == (SAFE_ON, 0)
    assert evs[g + 1] == (SAFE_OFF, 0)
    assert evs[g + 2] == (CTRL_WR, 0x04)
    assert evs[g + 3] == (CTRL_WR, 0x00)     # the resume begin() starts here
    assert evs[g + 4] == (CTRL_WR, 0x05)


# ---- erase ------------------------------------------------------------------

def test_erase_is_wren_then_sector_erase_then_status_poll(drv):
    # Distinct discard/status bytes: the engine's first read is the pipeline
    # byte and MUST be discarded — 0xA4 has WIP set, so a driver that acted
    # on the discard byte would keep polling (or time out) instead of
    # succeeding on this single poll.
    drv.push8(0xA4, 0x00)
    assert drv.erase(0x2000, 0x1000) == 0
    tx = transactions(drv.events())
    assert [t["op"] for t in tx] == [0xFF, 0x06, 0x20, 0x05]
    assert tx[1]["out"] == []                       # WREN carries no payload
    assert tx[2]["out"] == addr_bytes(0x2000)       # big-endian, 3 bytes
    assert tx[3]["inn"] == [0xA4, 0x00]             # exactly one poll


def test_wait_acts_on_the_status_byte_not_the_discard(drv):
    # Discard bytes read "ready" while the real status still has WIP set for
    # two polls: a driver that swapped the two reads would return after the
    # first iteration. The correct one polls exactly three times.
    drv.push8(0x00, 0x01, 0x00, 0x01, 0xA4, 0x00)
    assert drv.erase(0x2000, 0x1000) == 0
    polls = [t for t in transactions(drv.events()) if t["op"] == 0x05]
    assert len(polls) == 3
    assert polls[-1]["inn"] == [0xA4, 0x00]


def test_erase_never_uses_page_or_block_erase(drv):
    # The DEFAULT builds (both families) are sector-erase only; page erase is
    # opt-in per family via OB_FLASH_PAGE_ERASE — see the pe_drv tests below.
    assert drv.erase(0x2000, 0x2000) == 0           # two sectors
    ops = [t["op"] for t in transactions(drv.events())]
    assert ops.count(0x20) == 2
    assert 0x81 not in ops and 0xD8 not in ops      # CH592A page-erase hang
    txs = [t for t in transactions(drv.events()) if t["op"] == 0x20]
    assert txs[0]["out"] == addr_bytes(0x2000)
    assert txs[1]["out"] == addr_bytes(0x3000)


# ---- page erase (OB_FLASH_PAGE_ERASE=1, ch59x only) -------------------------

@pytest.fixture
def pe_drv():
    """The ch59x driver built with OB_FLASH_PAGE_ERASE=1 — 256 B page erase
    (0x81). Gate/resume params are the ch59x ones; only the erase op differs."""
    d = Drv("ch59x", so="ch59x_pageerase")
    d.reset()
    return d


def test_page_erase_is_wren_then_0x81_then_status_poll(pe_drv):
    pe_drv.push8(0xA4, 0x00)                         # one poll, as the 0x20 test
    assert pe_drv.erase(0x2000, 0x100) == 0          # exactly one 256 B page
    tx = transactions(pe_drv.events())
    assert [t["op"] for t in tx] == [0xFF, 0x06, 0x81, 0x05]
    assert 0x20 not in [t["op"] for t in tx]
    assert tx[2]["out"] == addr_bytes(0x2000)        # big-endian address
    assert tx[3]["inn"] == [0xA4, 0x00]


def test_page_erase_splits_a_4k_span_into_16_pages(pe_drv):
    assert pe_drv.erase(0x2000, 0x1000) == 0         # 4 KiB = 16 × 256 B
    ops = [t["op"] for t in transactions(pe_drv.events())]
    assert ops.count(0x81) == 16
    assert 0x20 not in ops and 0xD8 not in ops
    pages = [t for t in transactions(pe_drv.events()) if t["op"] == 0x81]
    assert pages[0]["out"] == addr_bytes(0x2000)
    assert pages[1]["out"] == addr_bytes(0x2100)     # +256
    assert pages[-1]["out"] == addr_bytes(0x2000 + 15 * 256)


def test_page_erase_rejects_a_non_256_aligned_length(pe_drv):
    # 0x800 spans two 256 B pages fine; 0x080 (128 B) is sub-page — rejected.
    assert pe_drv.erase(0x2000, 0x080) == E_ERASE_PARAM
    assert pe_drv.erase(0x2040, 0x100) == E_ERASE_PARAM   # addr not 256-aligned


def test_erase_timeout_reports_and_still_closes_the_gate(drv):
    drv.reset(rd8_default=0x01)                     # WIP never clears
    assert drv.erase(0x2000, 0x1000) == E_ERASE_TIMEOUT
    evs = drv.events()
    polls = [t for t in transactions(evs) if t["op"] == 0x05]
    assert len(polls) == WAIT_ITERS                 # OB_CPU_HZ-derived bound
    assert glob_writes(evs)[-1] == 0x00             # gate closed on failure


# ---- write ------------------------------------------------------------------

def test_write_streams_words_with_four_pulses_each(drv):
    data = bytes(range(48))
    assert drv.write(0x2000, data) == 0
    txs = transactions(drv.events())
    progs = [t for t in txs if t["op"] == 0x02]
    assert len(progs) == 1                          # no page crossed
    assert progs[0]["out"] == addr_bytes(0x2000)
    assert len(progs[0]["w32"]) == 12               # 48 bytes = 12 words
    assert progs[0]["pulses"] == 48                 # 4 CTRL=0x15 per word
    words = [int.from_bytes(data[i:i + 4], "little") for i in range(0, 48, 4)]
    assert progs[0]["w32"] == words
    # WREN precedes each program; a wait follows the page.
    ops = [t["op"] for t in txs]
    assert ops == [0xFF, 0x06, 0x02, 0x05]


def test_write_splits_at_the_256_byte_page_boundary(drv):
    # 48 bytes at 0xXXF0: 16 bytes to the boundary, 32 beyond.
    assert drv.write(0x20F0, bytes(48)) == 0
    txs = transactions(drv.events())
    progs = [t for t in txs if t["op"] == 0x02]
    assert [p["out"] for p in progs] == [addr_bytes(0x20F0),
                                         addr_bytes(0x2100)]
    assert [len(p["w32"]) for p in progs] == [4, 8]
    assert [t["op"] for t in txs] == [0xFF, 0x06, 0x02, 0x05,
                                      0x06, 0x02, 0x05]


def test_write_ending_exactly_on_a_page_boundary_is_one_program(drv):
    assert drv.write(0x20C0, bytes(64)) == 0
    progs = [t for t in transactions(drv.events()) if t["op"] == 0x02]
    assert len(progs) == 1 and len(progs[0]["w32"]) == 16


def test_write_timeout_mid_stream_closes_the_gate(drv):
    drv.reset(rd8_default=0x01)
    assert drv.write(0x2000, bytes(8)) == E_WRITE_TIMEOUT
    assert glob_writes(drv.events())[-1] == 0x00


# ---- verify -----------------------------------------------------------------

def test_verify_clocks_every_byte_and_compares_each_word(drv):
    # The mock's R32 register assembles from the last four clocked bytes,
    # like the silicon's word buffer — so this passes ONLY if the driver
    # compares at exactly the fourth byte of each word. Any phase shift
    # reads a partially-assembled word and mismatches.
    data = bytes(range(1, 17))
    drv.push8(*data)                                 # the flash's content
    assert drv.verify(0x2000, data) == 0
    txs = transactions(drv.events())
    rd = [t for t in txs if t["op"] == 0x0B][0]
    assert rd["out"] == addr_bytes(0x2000) + [0, 0]  # 3 addr + 2 dummy
    assert len(rd["inn"]) == 16                      # one clock per byte
    assert len(rd["r32"]) == 4                       # one compare per word
    words = [int.from_bytes(data[i:i + 4], "little") for i in range(0, 16, 4)]
    assert rd["r32"] == words                        # assembled at phase 3
    assert [t["op"] for t in txs] == [0xFF, 0x0B]    # read class: no WREN


def test_verify_mismatch_reports_and_closes(drv):
    data = bytes(range(1, 17))
    flash = bytearray(data)
    flash[9] ^= 0x80                                 # corrupt the third word
    drv.push8(*flash)
    assert drv.verify(0x2000, data) == E_VERIFY_MISMATCH
    evs = drv.events()
    rd = [t for t in transactions(evs) if t["op"] == 0x0B][0]
    assert len(rd["r32"]) == 3                       # stopped at the mismatch
    assert glob_writes(evs)[-1] == 0x00              # closed on failure


# ---- uid + rom info ---------------------------------------------------------

def test_uid_is_the_archives_xor_fold(drv):
    raw = list(range(0x10, 0x20))
    drv.push8(*raw)
    got = drv.uid()
    exp = bytes(raw[7 - j] ^ raw[15 - j] for j in range(8))
    assert got[:8] == exp
    assert got[8:] == b"\xcc\xcc\xcc\xcc\xdd\xdd\xdd\xdd"  # buf[2..3] kept
    txs = transactions(drv.events())
    rd = [t for t in txs if t["op"] == 0x4B][0]
    assert rd["out"] == [0, 0, 0, 0, 0]              # read class: 5 clocks
    assert 0x06 not in [t["op"] for t in txs]        # and no WREN


def test_rom_info_mac_window_stores_word_plus_halfword(drv):
    raw = (0xA1B2C3D4).to_bytes(4, "little") + (0xE5F60789).to_bytes(4, "little")
    drv.push8(*raw)                                  # 8 clocked info bytes
    got = drv.rom_info(0x3F018)                      # bit 13 set: MAC window
    assert got[0:4] == (0xA1B2C3D4).to_bytes(4, "little")
    assert got[4:6] == (0xE5F60789).to_bytes(4, "little")[:2]
    assert got[6:8] == (0x22222222).to_bytes(4, "little")[2:]  # untouched
    rd = [t for t in transactions(drv.events()) if t["op"] == 0x0B][0]
    assert rd["out"][:3] == addr_bytes(0x3F018 | drv.fam["info_or"])
    assert len(rd["inn"]) == 8


def test_rom_info_plain_window_stores_two_words(drv):
    raw = (0xA1B2C3D4).to_bytes(4, "little") + (0xE5F60789).to_bytes(4, "little")
    drv.push8(*raw)
    got = drv.rom_info(0x3C000)                      # bit 13 clear
    assert got[0:4] == (0xA1B2C3D4).to_bytes(4, "little")
    assert got[4:8] == (0xE5F60789).to_bytes(4, "little")


# ---- parameter rejection ----------------------------------------------------

@pytest.mark.parametrize("call,rc", [
    (lambda d: d.erase(0x2001, 0x1000), E_ERASE_PARAM),
    (lambda d: d.erase(0x2000, 0x0800), E_ERASE_PARAM),
    (lambda d: d.erase(0x2000, 0x1800), E_ERASE_PARAM),  # aligned, oversize
    (lambda d: d.erase(0x2000, 0x1001), E_ERASE_PARAM),
    (lambda d: d.erase(0x2000, 0), E_ERASE_PARAM),
    (lambda d: d.erase(0xFFFFF000, 0x2000), E_ERASE_PARAM),
    (lambda d: d.write(0x2002, bytes(8)), E_WRITE_PARAM),
    (lambda d: d.write(0x2000, bytes(6)), E_WRITE_PARAM),
    (lambda d: d.write(0x2000, b""), E_WRITE_PARAM),
    (lambda d: d.verify(0x2000, bytes(6)), E_VERIFY_PARAM),
])
def test_bad_parameters_fail_before_touching_the_gate(drv, call, rc):
    assert call(drv) == rc
    assert drv.events() == []                        # gate never opened


def test_unaligned_buffer_is_rejected(drv):
    raw = (ctypes.c_uint8 * 9)()
    unaligned = ctypes.byref(raw, 1)
    assert drv.lib.ob_ch5xx_flash_write(0x2000, unaligned, 8) == E_WRITE_PARAM
    assert drv.lib.ob_ch5xx_flash_verify(0x2000, unaligned, 8) == E_VERIFY_PARAM
    assert drv.events() == []


# ---- error codes ------------------------------------------------------------

def test_every_error_code_has_a_nonzero_low_byte():
    """The low byte is the E_FLASH wire detail; 0 is reserved for the core's
    generation-ceiling response (docs/PROTOCOL.md)."""
    for code in (E_ERASE_PARAM, E_ERASE_TIMEOUT, E_WRITE_PARAM,
                 E_WRITE_TIMEOUT, E_VERIFY_PARAM, E_VERIFY_MISMATCH):
        assert code & 0xFF != 0
