#!/usr/bin/env python3
"""Compose a whole-chip factory image: OpenBoot, 0x00 pad, application, and
(by default) the slot A boot record that makes it bootable.

The application links at OB_APP_BASE and the bootloader owns everything below
it, so the image is the OpenBoot binary padded out to that address followed by
the application binary. The application therefore sits at the same file offset
as its load address, and the whole image can be written at flash address 0.

With --bless-capacity the image also carries slot A's boot record, padded out
to the address the bootloader reads it from. Without it, a factory-programmed
part comes up in the BOOTLOADER - nothing is bootable until a host runs
`openboot bless` - which is not what "program a blank part on the line and ship
it" means. Composing the record here needs no device and no host tool: its
fields are the image length and CRC, both known at compose time.

The pad byte MUST be 0x00, never 0xFF: on CH5xx, programming 0xFF programs
nothing, so an 0xFF pad would never land in flash and a post-flash readback
compare would fail across the pad region.

The cost of blessing is size. The record sits at the top of slot A, so the
image spans everything below it: about 224 KiB on ch592 and 116 KiB on a
dongle ch570, nearly all zeros. That is the price of ONE contiguous blob that
any programmer can write at address 0.
"""

from __future__ import annotations

import argparse
from functools import lru_cache
import os
from pathlib import Path
import re
import struct
import sys
import tempfile
import zlib

PROTOCOL_H = Path(__file__).resolve().parent.parent / "protocol" / "openboot_protocol.h"


@lru_cache(maxsize=1)
def _header_text() -> str:
    """The header is a fixed input, so read it once and say so — several
    constants are pulled from it at import and each was re-reading the file."""
    return PROTOCOL_H.read_text()


def header_const(name: str) -> int:
    """A numeric #define from the protocol header — the one source of truth.

    Read from the header rather than the generated tests/ob_consts.py: a build
    tool importing from the test tree is the wrong direction, and this is the
    same anchored extraction firmware/Makefile uses for OB_BOOTREQ_ADDR."""
    match = re.search(
        rf"^#define\s+{re.escape(name)}\s+(0x[0-9A-Fa-f]+|\d+)u?\b",
        _header_text(),
        re.M,
    )
    if match is None:
        raise SystemExit(f"no '#define {name} <numeric>' in {PROTOCOL_H}")
    return int(match.group(1), 0)


def app_base() -> int:
    return header_const("OB_APP_BASE")


# OpenBoot owns everything below the application's load address.
OPENBOOT_REGION_BYTES = app_base()

RECORD_MAGIC = header_const("OB_RECORD_MAGIC")
RECORD_SIZE = header_const("OB_BOOT_RECORD_SIZE")
RECORD_RSVD = header_const("OB_RECORD_RSVD_BYTES")
RECORD_CRC_LEN = header_const("OB_RECORD_CRC_LEN")

# A factory part has never been updated, so its first record is generation 1.
# ob_record_load() rejects 0, and the first OTA update reads this and claims 2.
FACTORY_GENERATION = 1


def boot_record(img_len: int, img_crc32: int, generation: int = FACTORY_GENERATION) -> bytes:
    """The 32-byte OBR2 record the bootloader validates for a slot.

    Byte-for-byte what ob_record_store() would have written after a COMMIT of
    the same image, so a blessed factory image is indistinguishable from one
    the host tool flashed and attested."""
    body = struct.pack("<IIII", RECORD_MAGIC, generation, img_len, img_crc32)
    body += b"\x00" * RECORD_RSVD
    if len(body) != RECORD_CRC_LEN:
        raise ValueError(
            f"record body is {len(body)} bytes, header says the CRC covers "
            f"{RECORD_CRC_LEN}")
    record = body + struct.pack("<I", zlib.crc32(body))
    if len(record) != RECORD_SIZE:
        raise ValueError(
            f"record is {len(record)} bytes, header says {RECORD_SIZE}")
    return record


def compose_factory(openboot: bytes, app: bytes, app_max: int | None = None,
                    bless_capacity: int | None = None) -> bytes:
    if not 0 < len(openboot) <= OPENBOOT_REGION_BYTES:
        raise ValueError(
            f"OpenBoot image must be 1..{OPENBOOT_REGION_BYTES} bytes, "
            f"got {len(openboot)}")
    if not app:
        raise ValueError("application image is empty")
    # Without a bound the composer will happily emit an image that runs past
    # the end of the application region - and a whole-chip flash of it would
    # then write over whatever lives above.
    if app_max is not None and len(app) > app_max:
        raise ValueError(
            f"application is {len(app)} bytes, exceeds the {app_max}-byte "
            f"application region")
    head = openboot + b"\x00" * (OPENBOOT_REGION_BYTES - len(openboot))
    if bless_capacity is None:
        return head + app

    # img_len must be a whole number of flash words, so the recorded image is
    # the application rounded up. The pad is part of what the CRC covers, so
    # it has to be in the file too - and 0x00 for the same reason as the pad
    # above.
    #
    # The app_max check above used the UNPADDED length, which is right: when
    # blessing, the capacity below is the binding bound and it is strictly
    # tighter. A slot is half the region rounded down to an erase block, less
    # the block its record owns, so bless_capacity < app_max always and the
    # padding cannot carry the image past the region.
    image = app + b"\x00" * (-len(app) % 4)
    if len(image) > bless_capacity:
        raise ValueError(
            f"application is {len(image)} bytes (padded), exceeds the "
            f"{bless_capacity}-byte capacity of slot A; the top erase block "
            f"of the slot belongs to its boot record")
    record = boot_record(len(image), zlib.crc32(image))
    return head + image + b"\x00" * (bless_capacity - len(image)) + record


def write_atomic(path: Path, data: bytes) -> None:
    """Replace in one step: an interrupted compose must not leave a short image
    that the next `make` treats as up to date and someone then flashes.

    The temporary file is staged beside the destination so the replace is on
    one filesystem; the parent is created first so a standalone run against a
    fresh output directory reports a composition result rather than a
    FileNotFoundError from the staging call."""
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp = tempfile.mkstemp(dir=path.parent, prefix=f".{path.name}.")
    try:
        with os.fdopen(fd, "wb") as handle:
            handle.write(data)
        os.chmod(tmp, 0o644)
        os.replace(tmp, path)
    except BaseException:
        os.unlink(tmp)
        raise


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--openboot", type=Path, required=True,
                        help="OpenBoot bootloader binary, loaded at 0")
    parser.add_argument("--app", type=Path, required=True,
                        help="application binary, loaded at OB_APP_BASE")
    parser.add_argument("--app-end", default=None,
                        help="exclusive end of the application region, e.g. "
                             "0x00070000; the largest usable application "
                             "follows from it. Omit to skip the bound.")
    parser.add_argument("--bless-capacity", default=None,
                        help="usable image bytes in slot A (slot size minus "
                             "the erase block its record owns). Given, the "
                             "image carries slot A's boot record and a "
                             "factory-programmed part boots the application "
                             "straight away. Omitted, the part comes up in "
                             "the bootloader awaiting a bless.")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    try:
        # The composer already owns the region base (OB_APP_BASE, read from
        # the protocol header), so the caller passes only the end and the
        # bound follows. Keeps hex parsing in Python rather than in a shell
        # arithmetic expansion.
        app_max = None
        if args.app_end is not None:
            app_max = int(args.app_end, 0) - OPENBOOT_REGION_BYTES
        capacity = (int(args.bless_capacity, 0)
                    if args.bless_capacity is not None else None)
        factory = compose_factory(args.openboot.read_bytes(),
                                  args.app.read_bytes(), app_max, capacity)
    except ValueError as exc:
        print(f"factory composition failed: {exc}", file=sys.stderr)
        return 2
    write_atomic(args.output, factory)
    how = "bootable (slot A record included)" if args.bless_capacity else \
        "NOT bootable — needs `openboot bless` after programming"
    print(f"factory image: {args.output} ({len(factory)} B) — {how}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
