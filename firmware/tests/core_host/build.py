#!/usr/bin/env python3
"""Build the host-native test libraries: the REAL core sources compiled
against the mock port into firmware/build/host/ob_{ch57x,ch59x}.so.

main.c is excluded (it needs a transport) and is syntax-checked so it cannot
rot. The compiler selection is HOST_CC, then CC, then cc.
"""
import os
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent           # firmware/tests/core_host
FW = HERE.parent.parent                          # firmware
ROOT = FW.parent                                 # repo root
OUT = FW / "build" / "host"

CORE_SRC = [FW / "core" / n for n in ("crc32.c", "boot_core.c", "boot_decision.c")]
HOST_SRC = [HERE / "ob_host.c"]
POLL_TEST = HERE / "test_poll_conversion.c"

CFLAGS = [
    "-std=c99", "-O2", "-g", "-Wall", "-Wextra", "-Werror",
    f"-I{FW / 'core'}", f"-I{HERE}",
    '-DOB_PORT_HEADER="ob_host_port.h"',
    "-DOB_TRANSPORT_ID=OB_TRANSPORT_ID_USB",
]

# ch57x_imagecrc is the same core with the opt-in boot-time image check
# turned on. It gets a real library rather than a syntax check because
# OB_BOOT_IMAGE_CRC decides whether a device boots: it was compile-only, so
# nothing ever executed the comparison, and deleting it broke no test.
FAMILIES = {
    "ch57x": "-DOB_HOST_CH57X",
    "ch59x": "-DOB_HOST_CH59X",
    "ch57x_imagecrc": "-DOB_HOST_CH57X -DOB_BOOT_IMAGE_CRC=1",
}

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
    POLL_TEST,
    ROOT / "protocol" / "openboot_protocol.h",
    Path(__file__).resolve(),
]


def _run(cmd):
    subprocess.run([str(c) for c in cmd], check=True)


def host_compiler():
    return os.environ.get("HOST_CC") or os.environ.get("CC") or "cc"


def build(cc=None):
    cc = cc or host_compiler()
    OUT.mkdir(parents=True, exist_ok=True)
    for fam, dflags in FAMILIES.items():
        so = OUT / f"ob_{fam}.so"
        _run([cc, *CFLAGS, *dflags.split(), "-fPIC", "-shared",
              *CORE_SRC, *HOST_SRC, "-o", so])
    # Compile-only gates for code the .so builds do not reach.
    _run([cc, *CFLAGS, "-DOB_HOST_CH59X", "-Dmain=ob_fw_main",
          "-fsyntax-only", FW / "core" / "main.c"])
    for interval, millis, expected in ((20, 50, 2500), (333, 1, 4), (1500, 1, 1)):
        _run([
            cc, "-std=c99", "-Wall", "-Wextra", "-Werror",
            f"-I{FW / 'core'}", f"-DOB_POLL_INTERVAL_US={interval}",
            f"-DOB_TEST_MS={millis}", f"-DOB_EXPECTED_POLLS={expected}",
            "-fsyntax-only", POLL_TEST,
        ])


def ensure_built(cc=None):
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
