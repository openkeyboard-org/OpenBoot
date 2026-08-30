#!/usr/bin/env python3
"""Assert the RAM self-containment of the flash driver in BUILT artifacts.

Everything in .highcode runs with XIP dead: from the moment a flash-controller
transaction opens until it ends, an instruction fetch from code flash hangs the
part mid-erase with the write gate open. Section placement alone does not
guarantee self-containment — the compiler can emit calls to libgcc millicode
(-msave-restore), out-of-line memcpy, or .rodata jump tables, all of which live
in flash. flash_ch5xx.c is built with -mno-save-restore -fno-jump-tables to
prevent that; this gate proves the result rather than trusting the flags.

Two checks, both against real artifacts:

Object mode (--object): the driver object may place code and data ONLY in
.highcode.* sections, and every relocation must target a symbol defined in a
.highcode section of the same object — zero external references. This is exact
(relocations cannot hide), but meaningless under LTO, where the object is not
what gets linked.

ELF mode (--elf): scan the linked image's disassembly between
_highcode_vma_start and _highcode_vma_end; every jal/branch target and every
auipc-computed page must land at or above RAM (0x20000000 — MMIO higher still
also passes). This survives LTO cloning/inlining. Known limit: an absolute
lui-materialized flash data address is indistinguishable from a small constant
and is not flagged — the object-mode section check covers that case, and both
modes normally run together.
"""
import argparse
import re
import subprocess
import sys

RAM_BASE = 0x20000000

# Mnemonics whose printed operand is an absolute code target in a linked ELF.
DIRECT_JUMPS = ("jal", "j", "beq", "bne", "blt", "bge", "bltu", "bgeu",
                "beqz", "bnez", "blez", "bgez", "bltz", "bgtz", "ble",
                "bgt", "bleu", "bgtu")


def run(argv):
    return subprocess.run(argv, capture_output=True, text=True,
                          check=True).stdout


def check_object(objdump, obj):
    bad = []
    # Sections: only .highcode.* may be allocatable and non-empty.
    for line in run([objdump, "-h", obj]).splitlines():
        f = line.split()
        # "Idx Name Size VMA LMA Off Algn" rows; flags on the following line.
        if len(f) >= 7 and f[0].isdigit():
            name, size = f[1], int(f[2], 16)
            if size == 0 or name.startswith(".highcode."):
                continue
            if name.startswith((".debug", ".comment", ".riscv.attributes")):
                continue
            bad.append(f"{obj}: section {name} is {size} B; the driver may "
                       "only emit .highcode.* sections")
    # Relocations: with the section gate above proving the object owns ONLY
    # .highcode sections, any local ".L" label is necessarily highcode-
    # resident and every absolute (*ABS*) target is a materialized constant
    # (MMIO addresses). What remains — named symbols not defined in a
    # .highcode section of this object — is exactly the dangerous set:
    # externals like __riscv_save_0 or memcpy that the linker would resolve
    # into flash .text.
    section = None
    for line in run([objdump, "-r", obj]).splitlines():
        m = re.match(r"RELOCATION RECORDS FOR \[(.+)\]:", line)
        if m:
            section = m.group(1)
            continue
        f = line.split()
        if (section is None or len(f) != 3 or not f[0].strip("0123456789abcdef")
                == "" or not section.startswith(".highcode")):
            continue
        # "<sym>", "<sym>+0x...", "*ABS*+0x...", ".L<n>", ".highcode.x+0x..."
        symbol = re.split(r"[+-]0x", f[2])[0]
        if symbol == "*ABS*":
            # An *ABS* relocation only exists to materialize an address
            # (integer constants fold without one), so the addend must be
            # RAM or MMIO — an absolute code-flash address would pass every
            # other check and die with XIP.
            m = re.search(r"\*ABS\*\+0x([0-9a-f]+)", f[2])
            if m and int(m.group(1), 16) < RAM_BASE:
                bad.append(f"{obj}: {section} materializes flash address "
                           f"{f[2]}")
            continue
        if symbol.startswith((".highcode", ".L")):
            continue
        if symbol in DEFINED_HIGHCODE:
            continue
        bad.append(f"{obj}: {section} relocates against '{symbol}', which is "
                   "not in .highcode — a call or data reference that dies "
                   "with XIP")
    return bad


def collect_highcode_symbols(objdump, obj):
    """Named symbols the object defines inside .highcode sections."""
    syms = set()
    for line in run([objdump, "-t", obj]).splitlines():
        # "<addr> <flags> <section> <size> <name>" with multi-token flags;
        # keying on the section token is robust across flag variants.
        f = line.split()
        if len(f) >= 4 and f[-1] != f[-2] and \
                any(t.startswith(".highcode") for t in f[1:-2]):
            syms.add(f[-1])
    return syms


def symbol_addr(nm, elf, name):
    for line in run([nm, str(elf)]).splitlines():
        f = line.split()
        if len(f) == 3 and f[2] == name:
            return int(f[0], 16)
    return None


def check_elf(objdump, nm, elf):
    lo = symbol_addr(nm, elf, "_highcode_vma_start")
    hi = symbol_addr(nm, elf, "_highcode_vma_end")
    if lo is None or hi is None:
        return [f"{elf}: _highcode_vma_start/_highcode_vma_end not found"]
    bad = []
    scanned = 0
    insn = re.compile(r"^\s*([0-9a-f]+):\s+[0-9a-f]+\s+(\S+)\s*(.*)$")
    for line in run([objdump, "-d", str(elf)]).splitlines():
        m = insn.match(line)
        if not m:
            continue
        pc = int(m.group(1), 16)
        if not lo <= pc < hi:
            continue
        scanned += 1
        mnem, ops = m.group(2), m.group(3)
        if mnem in DIRECT_JUMPS:
            tm = re.search(r"\b([0-9a-f]+)\s+<", ops)
            if tm and int(tm.group(1), 16) < RAM_BASE:
                bad.append(f"{elf}: {pc:#x} {mnem} targets flash: {line.strip()}")
        elif mnem == "auipc":
            im = re.search(r"0x([0-9a-f]+)", ops)
            if im:
                page = (pc + (int(im.group(1), 16) << 12)) & 0xFFFFFFFF
                if page < RAM_BASE:
                    bad.append(f"{elf}: {pc:#x} auipc reaches flash: "
                               f"{line.strip()}")
    if scanned == 0:
        # A silent pass on an unparseable disassembly (wrong objdump, format
        # drift) must not look like a clean result.
        bad.append(f"{elf}: no instructions parsed in the .highcode range "
                   f"{lo:#x}..{hi:#x} — wrong objdump or format drift")
    return bad


DEFINED_HIGHCODE = set()


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--objdump", required=True, help="riscv objdump path")
    ap.add_argument("--nm", help="riscv nm path (required with --elf)")
    ap.add_argument("--object", action="append", default=[],
                    help="driver object(s) for the exact relocation check")
    ap.add_argument("--elf", action="append", default=[],
                    help="linked image(s) for the disassembly-range check")
    ap.add_argument("--ram-symbol", action="append", default=[],
                    help="symbol that must exist in every --elf and resolve "
                         "at or above RAM_BASE — pins the RAM residency of "
                         "noinline .highcode functions under LTO, where "
                         "section placement alone is not checked by anything")
    args = ap.parse_args()

    if args.elf and not args.nm:
        ap.error("--elf requires --nm")
    if not args.object and not args.elf:
        ap.error("nothing to check: pass --object and/or --elf")

    bad = []
    for obj in args.object:
        DEFINED_HIGHCODE.update(collect_highcode_symbols(args.objdump, obj))
    for obj in args.object:
        bad += check_object(args.objdump, obj)
    for elf in args.elf:
        bad += check_elf(args.objdump, args.nm, elf)
        for name in args.ram_symbol:
            addr = symbol_addr(args.nm, elf, name)
            if addr is None:
                bad.append(f"{elf}: required RAM symbol '{name}' not found "
                           "(inlined away? it must be noinline)")
            elif addr < RAM_BASE:
                bad.append(f"{elf}: '{name}' resolves to {addr:#x}, below "
                           f"RAM ({RAM_BASE:#x}) — it left .highcode")

    if bad:
        print("highcode self-containment VIOLATED:", file=sys.stderr)
        for b in bad:
            print(f"  {b}", file=sys.stderr)
        return 1
    print(f"highcode ok: {len(args.object)} object(s), {len(args.elf)} "
          "image(s); nothing reachable in flash during a transaction")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
