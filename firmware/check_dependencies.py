#!/usr/bin/env python3
"""Validate OpenBoot's pinned build inputs.

Checks, in order:

  1. Each pinned openwch EVT submodule is present, checked out at exactly the
     pinned revision, and has a clean worktree. Untracked files count as
     dirty: a stray header dropped into an SDK include path changes the build
     without moving HEAD, which is precisely what this gate exists to rule
     out.
  2. The MounRiver toolchain directory contains the tools the Makefile
     invokes. The compiler itself is only ever REPORTED on: an unexpected
     SHA-256 or an unvalidated major warns and the build proceeds. Only a
     missing or unrunnable tool fails.

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
from typing import NoReturn, Optional


REPO_ROOT = Path(__file__).resolve().parent.parent

# Submodule pins, relative to the repository root. Update these together with
# the .gitmodules checkout, never independently.
PINNED_SDKS = (
    ("third_party/openwch/ch570", "1e34c9450cd6df390153952c097cd09dd8a91aed"),
    ("third_party/openwch/ch592", "a46e0086f1ffb5e5502703970bff94888e67f4cb"),
)

# MounRiver "RISC-V Embedded GCC12" (xPack GNU RISC-V Embedded GCC 12.2.0).
# The SHA-256 records the compiler used for the reference builds. It is a
# reproducibility fingerprint, not an installation requirement.
PINNED_COMPILER_SHA256 = (
    "7f2d3c114b98fe9e48ac6abe6259a4574291a8e2aba960b21dce73528ece9ff2"
)

# Compiler majors OpenBoot has actually been built and validated with. Anything
# else warns and proceeds (see the module docstring); this is a statement about
# what has been qualified, not a permission list.
SUPPORTED_GCC_MAJORS = (12, 15)

# MounRiver renamed the tools at GCC15: riscv-wch-elf-* became
# riscv32-wch-elf-*. Probe for either so neither install needs symlinks.
TOOL_PREFIXES = ("riscv-wch-elf-", "riscv32-wch-elf-")

# What the Makefile invokes, prefix-less. Checked here so a missing tool fails
# at check-deps rather than mid-build (nm is only used by board-policy, which
# runs after a full matrix build).
REQUIRED_TOOLS = ("gcc", "objcopy", "size", "nm")


def fail(message: str) -> NoReturn:
    raise RuntimeError(message)


def warn(message: str) -> None:
    print(f"dependency check warning: {message}", file=sys.stderr)


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


def detect_tool_prefix(toolchain: Path) -> str:
    """Which riscv*-wch-elf- prefix this directory uses. Mirrors the probe in
    firmware/Makefile, which needs the same answer at parse time."""
    for prefix in TOOL_PREFIXES:
        candidate = toolchain / f"{prefix}gcc"
        if candidate.is_file() and os.access(candidate, os.X_OK):
            return prefix
    listed = " or ".join(f"{p}gcc" for p in TOOL_PREFIXES)
    fail(
        f"no MounRiver compiler in {toolchain}\n"
        f"expected {listed}\n"
        "point MRS_TOOLCHAIN at the bin directory of a MounRiver "
        "'RISC-V Embedded GCC' install"
    )


def validate_toolchain(toolchain_value: Optional[str]) -> None:
    if toolchain_value is None or not toolchain_value.strip():
        fail(
            "toolchain not specified; pass --toolchain BIN_DIR or set "
            "MRS_TOOLCHAIN to a MounRiver riscv-wch-elf bin directory"
        )
    toolchain = Path(toolchain_value).expanduser().resolve()
    if not toolchain.is_dir():
        fail(
            f"toolchain directory does not exist: {toolchain}\n"
            "install MounRiver Studio's 'RISC-V Embedded GCC12' or 'GCC15', "
            "or point MRS_TOOLCHAIN at an equivalent riscv-wch-elf bin "
            "directory"
        )
    prefix = detect_tool_prefix(toolchain)
    for name in REQUIRED_TOOLS:
        executable = toolchain / f"{prefix}{name}"
        if not executable.is_file() or not os.access(executable, os.X_OK):
            fail(f"MounRiver tool is missing or not executable: {executable}")
    compiler = toolchain / f"{prefix}gcc"
    digest = hashlib.sha256(compiler.read_bytes()).hexdigest()
    if digest != PINNED_COMPILER_SHA256:
        warn(
            f"compiler SHA-256 is {digest}\n"
            f"           expected {PINNED_COMPILER_SHA256}\n"
            f"for {compiler}\n"
            "build output may differ from the reference compiler"
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
    # Unparseable and unvalidated are the same situation: the compiler runs,
    # but it is not one the reference builds were made with. Say so, build on.
    match = re.search(r"(\d+)\.\d+\.\d+", version)
    if match is None or int(match.group(1)) not in SUPPORTED_GCC_MAJORS:
        supported = ", ".join(f"{m}.x" for m in SUPPORTED_GCC_MAJORS)
        warn(
            f"not a validated compiler ({version})\n"
            f"OpenBoot is built and tested with GCC {supported}\n"
            "building anyway; these are not the reference bytes"
        )


def main() -> int:
    parser = argparse.ArgumentParser(
        description="validate OpenBoot's pinned SDK submodules and toolchain"
    )
    parser.add_argument(
        "--toolchain",
        default=os.environ.get("MRS_TOOLCHAIN"),
        help="MounRiver riscv-wch-elf bin directory "
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
