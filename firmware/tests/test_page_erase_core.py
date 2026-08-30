"""End-to-end page-mode (OB_FLASH_PAGE_ERASE=1) core geometry test.

`ob_ch59x_pageerase.so` is the FULL core built at 256 B erase granularity
(the ch59x mock port lowers OB_FLASH_ERASE_BLOCK under the knob). This proves
in `make test` what the bench proves on silicon: the knob changes only the
erase granularity — HELLO `erase_block`, the ERASE alignment, and the
erased-block bitmap — while the A/B slot and record geometry stays identical
to the 4 KiB build. That invariance is the load-bearing property of the
record-block decoupling (a fixed 4 KiB OB_RECORD_BLOCK): if it held only for
the default build it would be untested, exactly the gap this closes.
"""
import pytest

from ob_native import get_device
from test_core_native import (
    CMD_ERASE, DET_ALIGN, E_ADDR, cmd_err, erase, hello, u32,
)


@pytest.fixture()
def dev_pe():
    d = get_device("ch59x_pageerase")
    d.reset()
    return d


@pytest.fixture()
def dev59():
    d = get_device("ch59x")
    d.reset()
    return d


def test_page_mode_advertises_a_256_byte_erase_block(dev_pe):
    assert int.from_bytes(hello(dev_pe)[16:20], "little") == 256


def test_slot_and_record_geometry_is_invariant(dev_pe, dev59):
    # The crux: page mode must NOT move any slot base or record, nor change
    # capacity — a page-erase and a sector-erase build share one flash map.
    for slot in (0, 1):
        assert dev_pe.slot_base(slot) == dev59.slot_base(slot)
        assert dev_pe.slot_record_addr(slot) == dev59.slot_record_addr(slot)
        assert dev_pe.slot_capacity(slot) == dev59.slot_capacity(slot)
    # The record still sits a fixed 4 KiB below the slot end in page mode.
    assert (dev_pe.slot_record_addr(0)
            == dev_pe.slot_base(0) + dev_pe.slot_capacity(0))
    assert (dev59.slot_record_addr(0) - dev59.slot_base(0)
            == dev59.slot_capacity(0) == dev_pe.slot_capacity(0))


def test_erase_alignment_is_256_in_page_mode(dev_pe):
    hello(dev_pe)                            # open a session
    base = dev_pe.slot_base(0)
    erase(dev_pe, base, 256)                 # one page — accepted
    erase(dev_pe, base + 256, 256)           # next page — accepted
    erase(dev_pe, base, 4096)                # a 4 KiB span is 16 pages — fine
    # Sub-page address and length are rejected on the 256 B boundary.
    cmd_err(dev_pe, CMD_ERASE, 5, u32(base + 128) + u32(256), E_ADDR, DET_ALIGN)
    cmd_err(dev_pe, CMD_ERASE, 6, u32(base) + u32(128), E_ADDR, DET_ALIGN)


def test_write_into_a_page_erased_block_is_allowed(dev_pe):
    # Erasing one 256 B page arms exactly that block for writes (bitmap keyed
    # at 256 B): a 4-byte write into it succeeds, one into an un-erased
    # neighbour is refused.
    from test_core_native import write, E_NOT_ERASED, CMD_WRITE, cmd_err as _e
    hello(dev_pe)                            # open a session
    base = dev_pe.slot_base(0)
    erase(dev_pe, base, 256)
    write(dev_pe, base, b"\xde\xad\xbe\xef")
    _e(dev_pe, CMD_WRITE, 7, u32(base + 256) + b"\x01\x02\x03\x04",
       E_NOT_ERASED)
