"""ctypes wrapper for the host-native libraries (firmware/build/host/ob_*.so).

Also carries the OBP frame helpers and the golden-vector loader shared by
the pytest suite.
"""
import ctypes
import importlib.util
import zlib
from ctypes import POINTER, byref, c_char_p, c_int32, c_uint32, c_void_p
from pathlib import Path

TESTS = Path(__file__).resolve().parent
FW = TESTS.parent
ROOT = FW.parent
GOLDEN = ROOT / "protocol" / "golden_frames.txt"

# Protocol numbers come from the generated mirror of the C header (see
# protocol/gen_protocol.py); the short names below are pure ALIASES, so no
# value is written down twice and nothing here can drift from the header.
from ob_consts import *  # noqa: F401,F403  (generated; see gen_protocol.py)
from ob_consts import (
    OB_APP_BASE, OB_BOOTREQ_MAGIC, OB_CMD_BOOT, OB_CMD_COMMIT, OB_CMD_CRC,
    OB_CMD_ERASE, OB_CMD_HELLO, OB_CMD_READ, OB_CMD_WRITE, OB_DET_ADDR_ALIGN,
    OB_DET_ADDR_RANGE, OB_DET_VERIFY_MISMATCH, OB_DET_VERIFY_NONSEQ,
    OB_DET_VERIFY_NORECORD, OB_E_ADDR, OB_E_ARG, OB_E_CMD, OB_E_CRC,
    OB_E_FLASH, OB_E_LEN, OB_E_NOT_ERASED, OB_E_PROTO, OB_E_STATE,
    OB_E_VERIFY, OB_FAMILY_CH570, OB_FAMILY_CH592, OB_FEAT_CRC_LIVE,
    OB_BOOT_RECORD_SIZE, OB_MAX_WRITE_DATA, OB_OK, OB_RECORD_MAGIC,
    OB_RECORD_RSVD_BYTES,
)

APP_START = OB_APP_BASE
BLOCK = 4096                      # erase granularity; a port fact, not wire
MAX_WRITE = OB_MAX_WRITE_DATA
BOOTREQ_MAGIC = OB_BOOTREQ_MAGIC
RECORD_MAGIC = OB_RECORD_MAGIC
RECORD_SIZE = OB_BOOT_RECORD_SIZE
RSVD = OB_RECORD_RSVD_BYTES   # zeroed filler covered by rec_crc32

OK, E_CRC, E_LEN, E_CMD = OB_OK, OB_E_CRC, OB_E_LEN, OB_E_CMD
E_STATE, E_ARG, E_ADDR = OB_E_STATE, OB_E_ARG, OB_E_ADDR
E_NOT_ERASED, E_FLASH, E_VERIFY, E_PROTO = (
    OB_E_NOT_ERASED, OB_E_FLASH, OB_E_VERIFY, OB_E_PROTO)
DET_RANGE, DET_ALIGN = OB_DET_ADDR_RANGE, OB_DET_ADDR_ALIGN
DET_MISMATCH, DET_NONSEQ, DET_NORECORD = (
    OB_DET_VERIFY_MISMATCH, OB_DET_VERIFY_NONSEQ, OB_DET_VERIFY_NORECORD)
CMD_HELLO, CMD_ERASE, CMD_WRITE, CMD_CRC = (
    OB_CMD_HELLO, OB_CMD_ERASE, OB_CMD_WRITE, OB_CMD_CRC)
CMD_COMMIT, CMD_BOOT, CMD_READ = OB_CMD_COMMIT, OB_CMD_BOOT, OB_CMD_READ

# Harness-side action codes (ob_host.c), not protocol constants.
ACT_NONE, ACT_RESET = 0, 2        # 1 was ACT_JUMP_APP (retired: reset-to-launch)

# Per-family harness facts. app_end and erased_word are build/port facts
# (the Makefile and the port headers own them, not the protocol), but family
# and features ARE protocol constants, so they come from the generated module
# rather than being written out again here.
PARAMS = {
    "ch57x": dict(app_end=0x3C000, erased_word=0xF3F9BDA9,
                  family=OB_FAMILY_CH570, features=0),
    "ch59x": dict(app_end=0x70000, erased_word=0xF3F9BDA9,
                  family=OB_FAMILY_CH592, features=OB_FEAT_CRC_LIVE),
    # Same core as ch57x with OB_BOOT_IMAGE_CRC=1, so the boot decision also
    # checks the image against the record it committed.
    "ch57x_imagecrc": dict(app_end=0x3C000, erased_word=0xF3F9BDA9,
                           family=OB_FAMILY_CH570, features=0),
}


def _ensure_built():
    build_py = TESTS / "core_host" / "build.py"
    spec = importlib.util.spec_from_file_location("ob_host_build", build_py)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    mod.ensure_built()


_ensure_built()


def frame(cmd: int, seq: int, payload: bytes = b"", flags: int = 0) -> bytes:
    body = bytes([cmd, seq, len(payload), flags]) + payload
    return body + zlib.crc32(body).to_bytes(4, "little")


def load_golden() -> dict:
    out = {}
    for line in GOLDEN.read_text().splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        name, hexbytes = line.split(":", 1)
        out[name.strip()] = bytes.fromhex(hexbytes.strip())
    return out


def erased_bytes(erased_word: int, addr: int, length: int) -> bytes:
    """XIP content of erased flash starting at addr."""
    return bytes((erased_word >> (8 * ((addr + i) & 3))) & 0xFF for i in range(length))


class OpenBootNative:
    def __init__(self, family: str):
        self.family = family
        p = PARAMS[family]
        self.app_end = p["app_end"]
        self.erased_word = p["erased_word"]
        self.family_id = p["family"]
        self.features = p["features"]
        L = ctypes.CDLL(str(FW / "build" / "host" / f"ob_{family}.so"))
        L.host_reset.restype = None
        L.host_power_cycle.restype = None
        L.host_frame.argtypes = [c_char_p, c_uint32, c_char_p,
                                 POINTER(c_uint32), POINTER(c_int32)]
        L.host_frame.restype = None
        L.host_flash_read.argtypes = [c_uint32, c_char_p, c_uint32]
        L.host_flash_read.restype = None
        L.host_record_raw.argtypes = [c_uint32, c_char_p]
        L.host_record_raw.restype = None
        L.host_set_record_raw.argtypes = [c_uint32, c_char_p]
        L.host_set_record_raw.restype = None
        L.host_jumped_to.restype = c_uint32
        L.host_write_flash.argtypes = [c_uint32, c_char_p, c_uint32]
        L.host_write_flash.restype = None
        L.ob_slot_base.argtypes = [c_uint32]
        L.ob_slot_base.restype = c_uint32
        L.ob_slot_record_addr.argtypes = [c_uint32]
        L.ob_slot_record_addr.restype = c_uint32
        L.ob_slot_capacity.argtypes = [c_uint32]
        L.ob_slot_capacity.restype = c_uint32
        L.ob_boot_select.argtypes = [POINTER(c_uint32)]
        L.ob_boot_select.restype = c_uint32
        L.host_set_fail_after.argtypes = [c_int32]
        L.host_set_fail_after.restype = None
        L.host_violations.restype = c_uint32
        L.host_op_total.restype = c_uint32
        L.host_set_bootreq.argtypes = [c_uint32]
        L.host_set_bootreq.restype = None
        L.host_get_bootreq.restype = c_uint32
        L.host_set_bootpin.argtypes = [c_int32]
        L.host_set_bootpin.restype = None
        L.host_set_silicon_app_end.argtypes = [c_uint32]
        L.host_set_silicon_app_end.restype = None
        L.host_set_f26.argtypes = [c_int32]
        L.host_set_f26.restype = None
        L.host_boot_decide_result.restype = c_int32
        L.ob_crc32.argtypes = [c_void_p, c_uint32]
        L.ob_crc32.restype = c_uint32
        L.host_set_uptime_ms.argtypes = [c_uint32]
        L.host_set_uptime_ms.restype = None
        L.ob_idle_elapsed.argtypes = [c_uint32, c_uint32, c_uint32]
        L.ob_idle_elapsed.restype = c_int32
        L.ob_ms_accumulate.argtypes = [POINTER(c_uint32), POINTER(c_uint32),
                                       c_uint32, c_uint32]
        L.ob_ms_accumulate.restype = None
        self._lib = L

    def idle_elapsed(self, start_ms: int, now_ms: int, timeout_ms: int) -> bool:
        return bool(self._lib.ob_idle_elapsed(start_ms, now_ms, timeout_ms))

    def ms_accumulate(self, ms: int, rem: int, delta_ticks: int,
                      ticks_per_ms: int) -> tuple[int, int]:
        """-> (ms, rem) after folding delta_ticks in."""
        c_ms, c_rem = c_uint32(ms), c_uint32(rem)
        self._lib.ob_ms_accumulate(byref(c_ms), byref(c_rem),
                                   delta_ticks, ticks_per_ms)
        return c_ms.value, c_rem.value

    def reset(self):
        self._lib.host_reset()

    def power_cycle(self):
        self._lib.host_power_cycle()

    def frame(self, data: bytes):
        """-> (response bytes, action)."""
        out = ctypes.create_string_buffer(64)
        outlen, act = c_uint32(0), c_int32(0)
        self._lib.host_frame(data, len(data), out, byref(outlen), byref(act))
        return out.raw[: outlen.value], act.value

    def flash_read(self, addr: int, length: int) -> bytes:
        buf = ctypes.create_string_buffer(length)
        self._lib.host_flash_read(addr, buf, length)
        return buf.raw

    def record_raw(self, slot: int = 0) -> bytes:
        buf = ctypes.create_string_buffer(OB_BOOT_RECORD_SIZE)
        self._lib.host_record_raw(slot, buf)
        return buf.raw

    def set_record_raw(self, raw: bytes, slot: int = 0):
        assert len(raw) == OB_BOOT_RECORD_SIZE
        self._lib.host_set_record_raw(slot, raw)

    # --- A/B slots ---------------------------------------------------
    def slot_base(self, slot: int) -> int:
        return self._lib.ob_slot_base(slot)

    def slot_record_addr(self, slot: int) -> int:
        return self._lib.ob_slot_record_addr(slot)

    def slot_capacity(self, slot: int = 0) -> int:
        return self._lib.ob_slot_capacity(slot)

    def boot_select(self) -> int:
        return self._lib.ob_boot_select(None)

    def highest_generation(self) -> int:
        generation = c_uint32()
        self._lib.ob_boot_select(byref(generation))
        return generation.value

    def write_flash(self, addr: int, data: bytes):
        self._lib.host_write_flash(addr, data, len(data))

    def jumped_to(self) -> int:
        return self._lib.host_jumped_to()

    def set_fail_after(self, n: int):
        self._lib.host_set_fail_after(n)

    def violations(self) -> int:
        return self._lib.host_violations()

    def op_total(self) -> int:
        return self._lib.host_op_total()

    def set_bootreq(self, v: int):
        self._lib.host_set_bootreq(v)

    def get_bootreq(self) -> int:
        return self._lib.host_get_bootreq()

    def set_bootpin(self, v: int):
        self._lib.host_set_bootpin(v)

    def set_silicon_app_end(self, v: int):
        """Pretend the die has a different app end than the build assumed
        (0 = an unrecognised chip id)."""
        self._lib.host_set_silicon_app_end(v)

    def set_f26(self, v: int):
        self._lib.host_set_f26(v)

    def boot_decide(self) -> int:
        """0 = jump to app, 1 = stay in bootloader."""
        return self._lib.host_boot_decide_result()

    def crc32_native(self, data: bytes) -> int:
        return self._lib.ob_crc32(data, len(data))

    def erased(self, addr: int, length: int) -> bytes:
        return erased_bytes(self.erased_word, addr, length)


_CACHE = {}


def get_device(family: str) -> OpenBootNative:
    if family not in _CACHE:
        _CACHE[family] = OpenBootNative(family)
    return _CACHE[family]
