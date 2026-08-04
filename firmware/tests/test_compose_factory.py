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


@pytest.mark.parametrize("size", [1, 100, 4080, 8191, 8192])
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


def test_write_atomic_creates_missing_parents(tmp_path):
    out = tmp_path / "nested" / "dir" / "factory.bin"

    compose_factory.write_atomic(out, b"payload")

    assert out.read_bytes() == b"payload"
    assert not list(out.parent.glob(".factory.bin.*")), "staging file left behind"
