#!/usr/bin/env python3
"""Assert the stay-in-bootloader strap policy in the BUILT images.

NO family ships a strap. Forcing a device into a bootloader with a pin is a
catastrophic-recovery mechanism and the silicon already provides one: the
mask-ROM ISP entry pin (PA1, which is also USB D+, on ch57x; PB22 on ch59x).
That path lives in ROM, cannot be broken by anything OpenBoot flashes, and is
the right tool for the job. OpenBoot does not layer a second strap on top.

Two reasons this is worth a gate rather than a comment. It is silent when
wrong — a strap on a pin the package does not bond (the old ch57x default was
PA4, absent from the CH570Q/CH572Q DFN10X3) reads its own pull-up forever and
simply never asserts, with no build error. And a strap that is compiled in but
unwired is worse than either choice, because it reads as a working escape
hatch in the docs and the bring-up checklist while doing nothing.

So check the compiled artifact, not the board file: `ob_bootpin_asserted()`
collapses to `return 0` when OB_BOOT_PIN_MASK is unset.

Only DEFAULT-board images are checked. A product board that genuinely wants a
user-facing "hold a key to enter update mode" may set the knob; pass --board
to skip.
"""
import argparse
import re
import subprocess
import sys
from pathlib import Path

# Family -> strap expected in the default build. Neither ships one.
POLICY = {"ch57x": False, "ch59x": False}
PORT = {"ch570": "ch57x", "ch572": "ch57x", "ch591": "ch59x", "ch592": "ch59x"}

# `return 0` is a couple of compressed instructions; a real implementation
# reads the GPIO direction/pull/input registers and is an order of magnitude
# bigger. The gap is wide enough that an exact threshold is not delicate.
STUB_MAX = 8


def symbol_size(nm: str, elf: Path, name: str):
    out = subprocess.run([nm, "--print-size", "--radix=d", str(elf)],
                         capture_output=True, text=True, check=True).stdout
    for line in out.splitlines():
        # "<addr> <size> <type> <name>"
        f = line.split()
        if len(f) == 4 and f[3] == name:
            return int(f[1])
    return None


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--nm", required=True, help="path to riscv-wch-elf-nm")
    ap.add_argument("--build", default="build", help="build directory root")
    ap.add_argument("--board", default="", help="non-default board: skip the check")
    args = ap.parse_args()

    if args.board and not args.board.startswith("generic-"):
        print(f"board policy: skipped (custom board {args.board})")
        return 0

    root = Path(args.build)
    elfs = sorted(root.glob("*/openboot-*.elf"))
    # Only default-board build dirs: a custom board suffixes them with "+name".
    elfs = [e for e in elfs if "+" not in e.parent.name]
    if not elfs:
        print(f"board policy: no images under {root}/ to check", file=sys.stderr)
        return 1

    bad = []
    checked = 0
    for elf in elfs:
        m = re.fullmatch(r"openboot-(ch\d{3})-(usb|uart)", elf.stem)
        if not m:
            bad.append(f"{elf.name}: unexpected OpenBoot artifact name")
            continue
        chip, transport = m.groups()
        family = PORT.get(chip)
        if family is None:
            bad.append(f"{elf.name}: unknown chip {chip}")
            continue
        checked += 1
        size = symbol_size(args.nm, elf, "ob_bootpin_asserted")
        if size is None:
            # Never pass silently on a toolchain/inlining change.
            bad.append(f"{elf.name}: ob_bootpin_asserted symbol not found; "
                       "the policy check cannot see the strap")
            continue
        has_strap = size > STUB_MAX
        want = POLICY[family]
        if has_strap != want:
            bad.append(
                f"{elf.name}: {family} expects "
                f"{'a strap' if want else 'NO strap'} but ob_bootpin_asserted "
                f"is {size} B ({'real' if has_strap else 'stub'}). "
                f"Check OB_BOOT_PIN_MASK in boards/generic-{family}.mk"
            )

    if bad:
        print("board policy VIOLATED:", file=sys.stderr)
        for b in bad:
            print(f"  {b}", file=sys.stderr)
        return 1
    print(f"board policy ok: {checked} images, no boot strap on any "
          "(ROM ISP is the recovery path)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
