"""The application companion's record validation, against the real thing.

openboot_app.c is one file applications drop in, and until this suite nothing
in the repo even compiled it. The tests here hold it to one standard: a record
is valid to the companion exactly when the BOOTLOADER would boot it. The
accepted record is therefore not hand-built — it is read back from the
simulated flash after the real core's COMMIT wrote it.
"""
import ctypes
from pathlib import Path

import pytest

from ob_native import get_device, OB_BOOT_RECORD_SIZE
from test_core_native import full_update

SO = Path(__file__).resolve().parent.parent / "build" / "host" / "ob_app.so"
# Must match the -D values core_host/build.py compiles ob_app.so with.
APP_SLOT_CAPACITY = 0x1D000 - 4096


@pytest.fixture(scope="module")
def companion():
    lib = ctypes.CDLL(str(SO))
    lib.openboot_record_valid.argtypes = [ctypes.c_char_p]
    lib.openboot_record_valid.restype = ctypes.c_int
    return lib


def valid(companion, rec: bytes) -> bool:
    assert len(rec) == OB_BOOT_RECORD_SIZE
    # Copied into a ctypes buffer rather than passed as raw bytes: the C side
    # reads uint32_t fields, and a Python bytes object's data is not
    # guaranteed aligned. malloc-backed buffers are.
    buf = ctypes.create_string_buffer(rec, len(rec))
    return companion.openboot_record_valid(buf) != 0


@pytest.fixture()
def committed_record():
    """32 bytes exactly as ob_record_store() left them in flash."""
    dev = get_device("ch57x")
    dev.reset()
    slot = full_update(dev, bytes(range(1, 65)))
    return dev.record_raw(slot)


def test_the_record_the_bootloader_writes_is_valid_to_the_app(companion, committed_record):
    """The interop claim itself: producer and consumer agree byte-for-byte."""
    assert valid(companion, committed_record)


def test_each_field_corruption_is_rejected(companion, committed_record):
    """Field-for-field parity with ob_record_load, each case re-sealed so it
    fails on the field it names — the discipline test_core_native.py's record
    tests learned the hard way (a stale CRC otherwise absorbs every case)."""
    import zlib

    body = committed_record[:28]

    def resealed(mutated: bytes) -> bytes:
        return mutated + zlib.crc32(mutated).to_bytes(4, "little")

    cases = {
        "bad magic": resealed(b"OBR1" + body[4:]),
        "generation 0": resealed(body[:4] + bytes(4) + body[8:]),
        "reserved nonzero": resealed(body[:16] + b"\x01" + body[17:]),
        "img_len 0": resealed(body[:8] + bytes(4) + body[12:]),
        "img_len misaligned": resealed(body[:8] + (65).to_bytes(4, "little") + body[12:]),
        "img_len past capacity": resealed(
            body[:8] + (APP_SLOT_CAPACITY + 4).to_bytes(4, "little") + body[12:]
        ),
        # NOT re-sealed: this one is the CRC check.
        "broken rec_crc32": committed_record[:-1] + b"\xAA",
    }
    for name, rec in cases.items():
        assert not valid(companion, rec), f"companion accepted a record with {name}"


def test_magic_alone_is_not_enough(companion, committed_record):
    """The defect that motivated this suite: openboot_get_record() used to
    return success on the magic word alone, so a record torn by a power cut
    mid-COMMIT — magic intact, CRC never written — was reported good."""
    torn = committed_record[:16] + b"\xFF" * 16     # tail still erased
    assert torn[:4] == b"OBR2", "torn record must keep its magic for this test"
    assert not valid(companion, torn)
