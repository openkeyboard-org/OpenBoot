"""Factory composition: OpenBoot, 0x00 pad, application.

These pin the composition byte-for-byte, because a factory flash compares
readback against exactly these bytes.
"""

import pytest

import compose_factory
from compose_factory import OPENBOOT_REGION_BYTES, compose_factory as compose

import ob_consts

APP = bytes((index * 37 + 11) & 0xFF for index in range(0x1000))


def test_region_matches_the_protocol_header():
    """The composer pads to the address the application links at, or the app
    lands somewhere the bootloader never jumps to."""
    assert OPENBOOT_REGION_BYTES == ob_consts.OB_APP_BASE


@pytest.mark.parametrize("size", [1, 100, 4080,
                                  OPENBOOT_REGION_BYTES - 1,
                                  OPENBOOT_REGION_BYTES])
def test_app_lands_at_exactly_the_app_base(size):
    openboot = bytes((index * 7 + 3) & 0xFF for index in range(size))

    factory = compose(openboot, APP)

    assert len(factory) == OPENBOOT_REGION_BYTES + len(APP)
    assert factory[:size] == openboot
    assert factory[OPENBOOT_REGION_BYTES:] == APP


def test_a_full_region_needs_no_pad():
    openboot = bytes(range(256)) * (OPENBOOT_REGION_BYTES // 256)
    assert len(openboot) == OPENBOOT_REGION_BYTES

    assert compose(openboot, APP) == openboot + APP


def test_pad_byte_is_0x00_not_0xff():
    """Regression pin: on CH5xx, programming 0xFF programs nothing, so an 0xFF
    pad would never land in flash and a factory readback compare would fail
    across the pad region."""
    openboot = b"\xAA" * 100

    pad = compose(openboot, APP)[len(openboot):OPENBOOT_REGION_BYTES]

    assert pad == b"\x00" * (OPENBOOT_REGION_BYTES - len(openboot))


@pytest.mark.parametrize("openboot,app", [
    (b"\x00" * (OPENBOOT_REGION_BYTES + 1), APP),   # oversized bootloader
    (b"", APP),                                     # empty bootloader
    (b"\xAA" * 100, b""),                           # empty application
])
def test_rejects_unusable_inputs(openboot, app):
    with pytest.raises(ValueError):
        compose(openboot, app)


def test_an_application_larger_than_the_region_is_refused():
    """Without this the composer emits an image running past the app region,
    and a whole-chip flash writes over whatever lives above it."""
    boot = b"\xAA" * 100

    compose(boot, b"\x11" * 64, 64)                  # exactly the bound is fine
    with pytest.raises(ValueError, match="exceeds"):
        compose(boot, b"\x11" * 65, 64)


def test_no_bound_given_means_no_bound_enforced():
    """The argument is optional so a standalone run still works; the Makefile
    always supplies it."""
    compose(b"\xAA" * 100, b"\x11" * 10_000_000)


def test_write_atomic_creates_missing_parents(tmp_path):
    out = tmp_path / "nested" / "dir" / "factory.bin"

    compose_factory.write_atomic(out, b"payload")

    assert out.read_bytes() == b"payload"
    assert not list(out.parent.glob(".factory.bin.*")), "staging file left behind"


# --- blessing -----------------------------------------------------------
# A factory part is programmed once, on a line, with no host tool in reach.
# The record has to be right in the file, so these pin its placement and
# contents. test_core_native.py drives a composed image through the REAL boot
# decision; these check the bytes.

CAPACITY = 0x1C000
BOOT = bytes((index * 3 + 1) & 0xFF for index in range(2048))


def record_of(image):
    return image[compose_factory.app_base() + CAPACITY:]


def test_blessing_puts_the_record_where_the_bootloader_reads_it():
    img = compose(BOOT, APP, bless_capacity=CAPACITY)
    assert len(img) == compose_factory.app_base() + CAPACITY + ob_consts.OB_BOOT_RECORD_SIZE
    rec = record_of(img)
    assert len(rec) == ob_consts.OB_BOOT_RECORD_SIZE
    assert int.from_bytes(rec[0:4], "little") == ob_consts.OB_RECORD_MAGIC


def test_the_record_describes_the_application_that_was_packed():
    import zlib
    img = compose(BOOT, APP, bless_capacity=CAPACITY)
    rec = record_of(img)
    assert int.from_bytes(rec[4:8], "little") == 1, "a factory part starts at generation 1"
    assert int.from_bytes(rec[8:12], "little") == len(APP)
    assert int.from_bytes(rec[12:16], "little") == zlib.crc32(APP)
    assert rec[16:28] == bytes(ob_consts.OB_RECORD_RSVD_BYTES), "reserved must be zero"
    assert int.from_bytes(rec[28:32], "little") == zlib.crc32(rec[:28])


def test_generation_zero_is_never_emitted():
    """ob_record_load() rejects it, so a record claiming it would leave a
    factory part unbootable in a way nothing here would notice."""
    assert compose_factory.FACTORY_GENERATION >= 1


@pytest.mark.parametrize("extra", [1, 2, 3])
def test_an_unaligned_application_is_padded_into_the_recorded_length(extra):
    """img_len must be a whole number of flash words. The pad is inside what
    the CRC covers, so it has to be in the file as well."""
    import zlib
    app = APP + bytes(extra)
    img = compose(BOOT, app, bless_capacity=CAPACITY)
    padded = app + b"\x00" * (-len(app) % 4)
    rec = record_of(img)
    assert int.from_bytes(rec[8:12], "little") == len(padded)
    assert int.from_bytes(rec[12:16], "little") == zlib.crc32(padded)
    base = compose_factory.app_base()
    assert img[base:base + len(padded)] == padded


def test_an_application_that_would_reach_the_record_is_refused():
    """The top erase block of the slot belongs to the record; an image that
    could reach it would be destroyed by its own first re-commit."""
    with pytest.raises(ValueError, match="capacity"):
        compose(BOOT, bytes(CAPACITY + 4), bless_capacity=CAPACITY)


def test_the_pad_between_application_and_record_is_0x00():
    img = compose(BOOT, APP, bless_capacity=CAPACITY)
    base = compose_factory.app_base()
    gap = img[base + len(APP):base + CAPACITY]
    assert gap and set(gap) == {0x00}


def test_not_blessing_leaves_the_old_layout_untouched():
    assert compose(BOOT, APP) == BOOT + bytes(
        compose_factory.app_base() - len(BOOT)) + APP


def test_real_geometries_keep_the_capacity_inside_the_region():
    """Every geometry the build derives leaves room for the slot and its
    record inside the application region."""
    for app_end in (0x30000, 0x3A000, 0x3C000, 0x70000):
        region = app_end - compose_factory.app_base()
        capacity = (region // 2 // 4096) * 4096 - 4096
        assert capacity + ob_consts.OB_BOOT_RECORD_SIZE <= region, f"{app_end:#x}"
        compose(BOOT, APP, app_max=region, bless_capacity=capacity)


def test_bounds_that_describe_different_geometries_are_refused():
    """The Makefile derives both bounds from one geometry, so they always
    agree — but they are independent arguments with independent CLI flags, and
    disagreeing ones used to emit an image running far past the region end
    (--app-end 0x2065 with --bless-capacity 0x1000 overran by 4027 bytes)."""
    with pytest.raises(ValueError, match="different geometries"):
        compose(BOOT, APP[:101], app_max=101, bless_capacity=0x1000)


def test_a_padded_application_cannot_cross_the_region_end():
    """101 bytes fits a 101-byte allowance; the image recorded for it is 104,
    so the padding is what crosses.

    The bounds here AGREE, which is the point: an earlier version of this test
    used app_max=101 with bless_capacity=104, and those describe different
    geometries, so it tripped that check and never reached the padded length
    it is named for."""
    app = APP[:101]

    tight = 100                       # the padded 104 will not fit this
    with pytest.raises(ValueError, match="capacity of slot A"):
        compose(BOOT, app, app_max=tight + ob_consts.OB_BOOT_RECORD_SIZE,
                bless_capacity=tight)

    fits = 104                        # exactly the padded length
    img = compose(BOOT, app, app_max=fits + ob_consts.OB_BOOT_RECORD_SIZE,
                  bless_capacity=fits)
    assert len(img) - compose_factory.app_base() == fits + ob_consts.OB_BOOT_RECORD_SIZE
