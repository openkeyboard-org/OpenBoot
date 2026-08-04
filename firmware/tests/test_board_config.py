"""The OpenDongle product board files must land their knobs in the
generated configuration header.

test_core_native.py proves the core honors a build bound tighter than the
silicon; these tests prove the board .mk actually PRODUCES that bound —
the include ordering in firmware/Makefile (per-chip APP_END default first,
board file second) is what makes the ch570 clamp effective, and nothing
else would notice if that ordering regressed.

Only the config-header rule runs (a printf + cmp rule), so no cross
toolchain is needed.
"""
import subprocess
from pathlib import Path

import pytest

FW = Path(__file__).resolve().parent.parent


def gen_config(chip: str, board: str) -> str:
    cfg_rel = f"build/{chip}-usb+{board}/openboot_config.h"
    subprocess.run(
        ["make", "--no-print-directory", "-C", str(FW),
         f"CHIP={chip}", "TRANSPORT=usb", f"BOARD={board}", cfg_rel],
        check=True, capture_output=True, text=True)
    return (FW / cfg_rel).read_text()


@pytest.mark.parametrize("chip,board,app_end", [
    ("ch570", "opendongle-ch570", "0x0003A000"),
    ("ch592", "opendongle-ch592", "0x00070000"),
])
def test_product_config_app_end(chip, board, app_end):
    cfg = gen_config(chip, board)
    assert f"#define OB_FLASH_APP_END {app_end}\n" in cfg


@pytest.mark.parametrize("chip,board", [
    ("ch570", "opendongle-ch570"),
    ("ch592", "opendongle-ch592"),
])
def test_product_config_boot_image_crc(chip, board):
    cfg = gen_config(chip, board)
    assert "#define OB_BOOT_IMAGE_CRC 1\n" in cfg


@pytest.mark.parametrize("bad", [
    "0x0003A800",   # not 4096-aligned
    "0x0003C000",   # past the silicon end
    "0x1000",       # below the app base
    "0xZZZ",        # not a number
])
def test_unusable_app_end_is_refused(bad):
    """The C side #errors on an overlapping region anyway; this is the
    friendlier message, and it also catches the cases C cannot see."""
    result = subprocess.run(
        ["make", "--no-print-directory", "-C", str(FW),
         "CHIP=ch570", "TRANSPORT=usb", f"OB_APP_END={bad}",
         "build/ch570-usb/openboot_config.h"],
        capture_output=True, text=True)

    assert result.returncode != 0
    assert "is not usable on ch570" in result.stderr


@pytest.mark.parametrize("chip,board", [
    ("ch570", "opendongle-ch570"),
    ("ch592", "opendongle-ch592"),
])
def test_product_boards_define_no_strap(chip, board):
    """Strap policy: product boards must not define OB_BOOT_PIN_MASK
    (board-policy's nm-based gate only checks the default boards)."""
    assert "OB_BOOT_PIN_MASK" not in gen_config(chip, board)
