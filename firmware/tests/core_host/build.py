#!/usr/bin/env python3
"""Build the host-native test libraries: the REAL core sources compiled
against the mock port into firmware/build/host/ob_{ch57x,ch59x}.so.

main.c is excluded (it needs a transport); it and the OB_BOOT_IMAGE_CRC
path are still syntax-checked so they cannot rot.
"""
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent           # firmware/tests/core_host
FW = HERE.parent.parent                          # firmware
ROOT = FW.parent                                 # repo root
OUT = FW / "build" / "host"

CORE_SRC = [FW / "core" / n for n in ("crc32.c", "boot_core.c", "boot_decision.c")]
HOST_SRC = [HERE / "ob_host.c"]

CFLAGS = [
    "-std=c99", "-O2", "-g", "-Wall", "-Wextra", "-Werror",
    f"-I{FW / 'core'}", f"-I{HERE}",
    '-DOB_PORT_HEADER="ob_host_port.h"',
    "-DOB_TRANSPORT_ID=OB_TRANSPORT_ID_USB",
]

FAMILIES = {"ch57x": "-DOB_HOST_CH57X", "ch59x": "-DOB_HOST_CH59X"}

DEPS = CORE_SRC + HOST_SRC + [
    HERE / "ob_host_port.h",
    HERE / "ob_host.h",
    FW / "core" / "crc32.h",
    FW / "core" / "boot_core.h",
    FW / "core" / "boot_decision.h",
    FW / "core" / "ob_xip.h",
    FW / "core" / "openboot_port.h",
    FW / "core" / "openboot_transport.h",
    FW / "core" / "main.c",
    ROOT / "protocol" / "openboot_protocol.h",
    Path(__file__).resolve(),
]


def _run(cmd):
    subprocess.run([str(c) for c in cmd], check=True)


def build(cc="cc"):
    OUT.mkdir(parents=True, exist_ok=True)
    for fam, dflag in FAMILIES.items():
        so = OUT / f"ob_{fam}.so"
        _run([cc, *CFLAGS, dflag, "-fPIC", "-shared",
              *CORE_SRC, *HOST_SRC, "-o", so])
    # Compile-only gates for code the .so builds don't reach.
    _run([cc, *CFLAGS, "-DOB_HOST_CH57X", "-DOB_BOOT_IMAGE_CRC=1",
          "-fsyntax-only", FW / "core" / "boot_decision.c"])
    _run([cc, *CFLAGS, "-DOB_HOST_CH59X", "-Dmain=ob_fw_main",
          "-fsyntax-only", FW / "core" / "main.c"])


def ensure_built(cc="cc"):
    sos = [OUT / f"ob_{fam}.so" for fam in FAMILIES]
    if all(so.exists() for so in sos):
        newest_src = max(p.stat().st_mtime for p in DEPS)
        if min(so.stat().st_mtime for so in sos) > newest_src:
            return
    build(cc)


if __name__ == "__main__":
    build()
    print(f"built {', '.join(f'ob_{f}.so' for f in FAMILIES)} in {OUT}")
    sys.exit(0)
