#!/usr/bin/env python3
"""Validate OpenBoot's pinned build inputs.

Checks, in order:

  1. Each pinned openwch EVT submodule is present, checked out at exactly the
     pinned revision, and has a clean worktree. Untracked files count as
     dirty: a stray header dropped into an SDK include path changes the build
     without moving HEAD, which is precisely what this gate exists to rule
     out.
  2. The MounRiver GCC12 toolchain directory contains the riscv-wch-elf tools
     the Makefile invokes, riscv-wch-elf-gcc is byte-identical to the pinned
     binary (SHA-256), and its --version reports the expected major.

Paths are resolved relative to this file, so it can be run from anywhere:

    python3 firmware/check_dependencies.py [--toolchain BIN_DIR]
                                           [--skip-toolchain]

Exit status: 0 on success, 2 on failure with an actionable message on stderr.
"""

from __future__ import annotations

import argparse
import hashlib
import os
from pathlib import Path
import re
import subprocess
import sys
from typing import Optional


REPO_ROOT = Path(__file__).resolve().parent.parent

# Submodule pins, relative to the repository root. Update these together with
# the .gitmodules checkout, never independently.
PINNED_SDKS = (
    ("third_party/openwch/ch570", "1e34c9450cd6df390153952c097cd09dd8a91aed"),
    ("third_party/openwch/ch592", "a46e0086f1ffb5e5502703970bff94888e67f4cb"),
)

# MounRiver "RISC-V Embedded GCC12" (xPack GNU RISC-V Embedded GCC 12.2.0).
# The SHA-256 pins the exact gcc binary; the major check below catches the
# cheaper failure mode of MRS_TOOLCHAIN pointing at some other install.
PINNED_COMPILER_SHA256 = (
    "7f2d3c114b98fe9e48ac6abe6259a4574291a8e2aba960b21dce73528ece9ff2"
)
REQUIRED_GCC_MAJOR = 12

# Every tool the Makefile invokes from $(MRS_TOOLCHAIN). Checked here so a
# missing tool fails at check-deps with a clear message instead of mid-build.
REQUIRED_TOOLS = (
    "riscv-wch-elf-gcc",
    "riscv-wch-elf-objcopy",
    "riscv-wch-elf-size",
)


def fail(message: str) -> None:
    raise RuntimeError(message)


def run_git(tree: Path, *args: str) -> str:
    try:
        result = subprocess.run(
            ["git", "-C", os.fspath(tree), *args],
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
    except (OSError, subprocess.CalledProcessError) as exc:
        fail(f"cannot inspect git checkout {tree}: {exc}")
    return result.stdout.strip()


def validate_sdk(relative: str, revision: str) -> None:
    sdk = REPO_ROOT / relative
    if not (sdk / "EVT").is_dir():
        fail(
            f"SDK submodule is missing or empty: {sdk}\n"
            "initialize submodules with: git submodule update --init --recursive"
        )
    actual = run_git(sdk, "rev-parse", "HEAD")
    if actual != revision:
        fail(
            f"SDK {sdk} is at {actual}, expected {revision}\n"
            "re-pin with: git submodule update --init --checkout "
            f"{relative}"
        )
    # Untracked files count as dirty (see the module docstring).
    status = run_git(sdk, "status", "--porcelain", "--untracked-files=normal")
    if status:
        listed = "\n  ".join(status.splitlines()[:10])
        fail(
            f"SDK checkout is dirty: {sdk}\n  {listed}\n"
            "revert or remove the changes so the build uses only pinned bytes"
        )


def validate_toolchain(toolchain_value: Optional[str]) -> None:
    if toolchain_value is None or not toolchain_value.strip():
        fail(
            "toolchain not specified; pass --toolchain BIN_DIR or set "
            "MRS_TOOLCHAIN to the MounRiver GCC12 bin directory"
        )
    toolchain = Path(toolchain_value).expanduser().resolve()
    if not toolchain.is_dir():
        fail(
            f"toolchain directory does not exist: {toolchain}\n"
            "install MounRiver Studio's 'RISC-V Embedded GCC12' or point "
            "MRS_TOOLCHAIN at an equivalent riscv-wch-elf bin directory"
        )
    for name in REQUIRED_TOOLS:
        executable = toolchain / name
        if not executable.is_file() or not os.access(executable, os.X_OK):
            fail(f"MounRiver tool is missing or not executable: {executable}")
    compiler = toolchain / "riscv-wch-elf-gcc"
    digest = hashlib.sha256(compiler.read_bytes()).hexdigest()
    if digest != PINNED_COMPILER_SHA256:
        fail(
            f"compiler SHA-256 is {digest}\n"
            f"           expected {PINNED_COMPILER_SHA256}\n"
            f"for {compiler}\n"
            "a different compiler produces different bytes in the 8 KiB "
            "region that has no self-update path; install the pinned GCC12 "
            "or, after qualifying a new one on hardware, update the pin here"
        )
    try:
        version = subprocess.run(
            [os.fspath(compiler), "--version"],
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        ).stdout.splitlines()[0]
    except (OSError, subprocess.CalledProcessError, IndexError) as exc:
        fail(f"cannot identify compiler {compiler}: {exc}")
    match = re.search(r"(\d+)\.\d+\.\d+", version)
    if match is None or int(match.group(1)) != REQUIRED_GCC_MAJOR:
        fail(
            f"compiler is not the required GCC {REQUIRED_GCC_MAJOR}.x: "
            f"{version}"
        )


def main() -> int:
    parser = argparse.ArgumentParser(
        description="validate OpenBoot's pinned SDK submodules and toolchain"
    )
    parser.add_argument(
        "--toolchain",
        default=os.environ.get("MRS_TOOLCHAIN"),
        help="MounRiver GCC12 bin directory "
        "(default: $MRS_TOOLCHAIN; required unless --skip-toolchain)",
    )
    parser.add_argument(
        "--skip-toolchain",
        action="store_true",
        help="check only the SDK submodules (for hosts without the cross "
        "toolchain, e.g. running the host-native tests)",
    )
    args = parser.parse_args()
    try:
        for relative, revision in PINNED_SDKS:
            validate_sdk(relative, revision)
        if not args.skip_toolchain:
            validate_toolchain(args.toolchain)
    except RuntimeError as exc:
        print(f"dependency check failed: {exc}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
