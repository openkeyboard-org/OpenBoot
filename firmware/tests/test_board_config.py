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


def gen_config(chip: str, board: str, transport: str = "usb") -> str:
    # Mirrors build_dir in firmware/Makefile: only a NON-default board gets a
    # +<board> suffix, so the default keeps the documented build/<chip>-<tr>
    # path. Getting this wrong just fails to find a target, loudly.
    suffix = "" if board.startswith("generic-") else f"+{board}"
    cfg_rel = f"build/{chip}-{transport}{suffix}/openboot_config.h"
    subprocess.run(
        ["make", "--no-print-directory", "-C", str(FW),
         f"CHIP={chip}", f"TRANSPORT={transport}", f"BOARD={board}", cfg_rel],
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


@pytest.mark.parametrize("chip,board", [
    ("ch570", "opendongle-ch570"),
    ("ch592", "opendongle-ch592"),
])
def test_product_boards_use_the_dongle_usb_identity(chip, board):
    """The product bootloader enumerates as the dongle, not as a separate
    device; the host separates them by HID usage page (PROTOCOL.md 12)."""
    cfg = gen_config(chip, board)

    assert "#define OB_USB_VID 0x0C45\n" in cfg
    assert "#define OB_USB_PID 0xFEFE\n" in cfg


def test_default_board_leaves_the_usb_identity_alone():
    """Absent knobs must emit no line at all, so usb_transport.c's #ifndef
    defaults win — the same contract as every other optional knob."""
    assert "OB_USB_" not in gen_config("ch592", "generic-ch59x")


@pytest.mark.parametrize("chip,board,size,b_base", [
    # Two equal slots, floored to the erase block. ch570 generic now spans all
    # of CodeFlash and halves exactly; the OpenDongle clamp to 0x3A000 keeps
    # OBP off the bond page and halves exactly too, at a smaller size.
    ("ch570", "generic-ch57x",       "0x0001D000", "0x0001F000"),
    ("ch570", "opendongle-ch570",    "0x0001C000", "0x0001E000"),
    ("ch591", "generic-ch59x",       "0x00017000", "0x00019000"),
    ("ch592", "opendongle-ch592",    "0x00037000", "0x00039000"),
])
def test_slot_geometry(chip, board, size, b_base):
    cfg = gen_config(chip, board)

    assert f"#define OB_SLOT_SIZE {size}\n" in cfg
    assert "#define OB_SLOT_A_BASE 0x00002000\n" in cfg
    assert f"#define OB_SLOT_B_BASE {b_base}\n" in cfg


@pytest.mark.parametrize("chip,board", [
    ("ch570", "opendongle-ch570"),
    ("ch592", "opendongle-ch592"),
])
def test_both_slots_fit_inside_the_app_region(chip, board):
    """The C side #errors on this too; catching it here says which board."""
    cfg = gen_config(chip, board)

    def val(name):
        line = next(ln for ln in cfg.splitlines()
                    if ln.startswith(f"#define {name} "))
        return int(line.split()[-1], 16)

    assert val("OB_SLOT_B_BASE") + val("OB_SLOT_SIZE") <= val("OB_FLASH_APP_END")
    assert val("OB_SLOT_A_BASE") + val("OB_SLOT_SIZE") == val("OB_SLOT_B_BASE")


@pytest.mark.parametrize("bad", [
    "0x0003A800",   # not 4096-aligned
    "0x0003D000",   # past the silicon end
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


def test_opencontroller_board_lands_its_knobs():
    """The keyboard module board is UART-transport with the PB12/PB13 remap;
    a regression in the board file or the Makefile include ordering would
    build a bootloader listening on the wrong pins (PA8/PA9) and brick the
    module's only wired update path."""
    cfg = gen_config("ch592", "opencontroller-ch592", transport="uart")
    assert "#define OB_TRANSPORT_ID OB_TRANSPORT_ID_UART\n" in cfg
    assert "#define OB_UART1_REMAP 1\n" in cfg
    assert "#define OB_BOOT_IMAGE_CRC 1\n" in cfg
    assert "#define OB_IDLE_TIMEOUT_MS 10000\n" in cfg
    # No OB_APP_END clamp: the module's bond lives in DataFlash, so the full
    # app region stays available.
    assert "#define OB_FLASH_APP_END 0x00070000\n" in cfg
