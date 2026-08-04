#!/usr/bin/env python3
"""Compose a whole-chip factory image: OpenBoot, 0x00 pad, application.

The application links at OB_APP_BASE and the bootloader owns everything below
it, so the image is the OpenBoot binary padded out to that address followed by
the application binary. The application therefore sits at the same file offset
as its load address, and the whole image can be written at flash address 0.

The pad byte MUST be 0x00, never 0xFF: on CH5xx, programming 0xFF programs
nothing, so an 0xFF pad would never land in flash and a post-flash readback
compare would fail across the pad region.
"""

from __future__ import annotations

import argparse
import os
from pathlib import Path
import re
import sys
import tempfile

PROTOCOL_H = Path(__file__).resolve().parent.parent / "protocol" / "openboot_protocol.h"


def app_base() -> int:
    """OB_APP_BASE from the protocol header — the one source of truth.

    Read from the header rather than the generated tests/ob_consts.py: a build
    tool importing from the test tree is the wrong direction, and this is the
    same anchored extraction firmware/Makefile uses for OB_BOOTREQ_ADDR."""
    match = re.search(
        r"^#define\s+OB_APP_BASE\s+(0x[0-9A-Fa-f]+)u?\b",
        PROTOCOL_H.read_text(),
        re.M,
    )
    if match is None:
        raise SystemExit(f"no '#define OB_APP_BASE 0x...' in {PROTOCOL_H}")
    return int(match.group(1), 16)


# OpenBoot owns everything below the application's load address.
OPENBOOT_REGION_BYTES = app_base()


def compose_factory(openboot: bytes, app: bytes, app_max: int | None = None) -> bytes:
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
    return openboot + b"\x00" * (OPENBOOT_REGION_BYTES - len(openboot)) + app


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
        factory = compose_factory(args.openboot.read_bytes(),
                                  args.app.read_bytes(), app_max)
    except ValueError as exc:
        print(f"factory composition failed: {exc}", file=sys.stderr)
        return 2
    write_atomic(args.output, factory)
    print(f"factory image: {args.output} ({len(factory)} B)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
