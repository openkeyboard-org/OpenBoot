"""The OpenDongle product board files must land their knobs in the
generated configuration header.

test_core_native.py proves the core honors a build bound tighter than the
silicon; these tests prove the board .mk actually PRODUCES that bound. The
board file is now included FIRST (it picks the chip), the per-chip APP_END
default runs next, and the board's OB_APP_END clamp re-overrides it after —
that ordering is what makes the ch570 clamp effective, and nothing else would
notice if it regressed.

Only the config-header rule runs (a printf + cmp rule), so no cross
toolchain is needed.
"""
import subprocess
from pathlib import Path

import pytest

FW = Path(__file__).resolve().parent.parent


def gen_config(chip: str, board: str, transport: str = "usb") -> str:
    # The board now picks the chip/transport, so the build is invoked by BOARD
    # alone and lands under build/<board>/. `chip`/`transport` stay in the
    # signature only because callers pass them as the parametrize values they
    # also assert against; they no longer drive the build.
    cfg_rel = f"build/{board}/openboot_config.h"
    subprocess.run(
        ["make", "--no-print-directory", "-C", str(FW), f"BOARD={board}", cfg_rel],
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


def test_page_erase_knob_defaults_off_and_lands_when_set():
    # Default off, always emitted (consumed with #if).
    assert "#define OB_FLASH_PAGE_ERASE 0\n" in gen_config("ch592", "generic-ch592-uart")
    # Explicit opt-in on a ch59x build lands as 1.
    cfg = subprocess.run(
        ["make", "--no-print-directory", "-C", str(FW),
         "CHIP=ch592", "TRANSPORT=uart", "OB_FLASH_PAGE_ERASE=1",
         "build/generic/openboot_config.h"],
        check=True, capture_output=True, text=True)
    assert "#define OB_FLASH_PAGE_ERASE 1\n" in \
        (FW / "build/generic/openboot_config.h").read_text()


@pytest.mark.parametrize("chip", ["ch570"])
def test_page_erase_is_ch592_only(chip):
    # Proven only on CH592F; ch570 (ch57x) has no page-erase command, so the
    # one other buildable chip is rejected. (ch572/ch591 are no longer build
    # targets at all — see test_dropped_chips_are_rejected.)
    result = subprocess.run(
        ["make", "--no-print-directory", "-C", str(FW),
         f"CHIP={chip}", "TRANSPORT=uart", "OB_FLASH_PAGE_ERASE=1",
         "build/generic/openboot_config.h"],
        capture_output=True, text=True)
    assert result.returncode != 0
    assert "OB_FLASH_PAGE_ERASE=1 is CH592-only" in result.stderr


@pytest.mark.parametrize("chip", ["ch572", "ch591"])
def test_dropped_chips_are_rejected(chip):
    """CH572/CH591 have no product and are no longer build targets, so the build
    must refuse them at chip-validation. The runtime wrong-variant safety and
    the host-side family enums that recognize this silicon are unaffected."""
    result = subprocess.run(
        ["make", "--no-print-directory", "-C", str(FW), f"CHIP={chip}",
         "build/generic/openboot_config.h"],
        capture_output=True, text=True)
    assert result.returncode != 0
    assert "CHIP must be one of {ch570 ch592}" in result.stderr


def test_page_erase_leaves_injected_slot_geometry_unchanged():
    # The record-block decoupling in the REAL build: OB_FLASH_PAGE_ERASE must
    # change only the erase granularity, never the Make-injected slot geometry
    # (OB_SLOT_SIZE / bases / app end) — that invariance is what keeps images
    # and factory blessings compatible across the knob.
    cfg_rel = "build/generic/openboot_config.h"
    keys = ("OB_SLOT_SIZE", "OB_SLOT_A_BASE", "OB_SLOT_B_BASE",
            "OB_FLASH_APP_END")

    def geometry(page_erase):
        subprocess.run(
            ["make", "--no-print-directory", "-C", str(FW), "CHIP=ch592",
             "TRANSPORT=uart", f"OB_FLASH_PAGE_ERASE={page_erase}", cfg_rel],
            check=True, capture_output=True, text=True)
        lines = (FW / cfg_rel).read_text().splitlines()
        return {k: next(l for l in lines if f"#define {k} " in l) for k in keys}

    off, on = geometry(0), geometry(1)
    assert off == on, f"slot geometry moved with the knob: {off} vs {on}"


@pytest.mark.parametrize("bad", ["2", "yes", "0 1"])
def test_page_erase_must_be_an_exact_boolean(bad):
    result = subprocess.run(
        ["make", "--no-print-directory", "-C", str(FW),
         "CHIP=ch592", "TRANSPORT=uart", f"OB_FLASH_PAGE_ERASE={bad}",
         "build/generic/openboot_config.h"],
        capture_output=True, text=True)
    assert result.returncode != 0
    assert "OB_FLASH_PAGE_ERASE must be 0 or 1" in result.stderr


@pytest.mark.parametrize("board,chip,transport", [
    ("opendongle-ch570",      "ch570", "USB"),
    ("opendongle-ch592",      "ch592", "USB"),
    ("opencontroller-ch592",  "ch592", "UART"),
    ("mk65mx-wireless-ch592", "ch592", "UART"),
    ("generic-ch570-uart",    "ch570", "UART"),
])
def test_boards_pin_their_chip_and_transport(board, chip, transport):
    """A board `override`s CHIP/TRANSPORT; prove both actually reach the config.
    Without this, a typo pointing a board at the wrong silicon or transport would
    still pass every knob test — those don't look at family/transport."""
    cfg = gen_config(chip, board)
    assert f"#define OB_CHIP_FAMILY OB_FAMILY_{chip.upper()}\n" in cfg
    assert f"#define OB_TRANSPORT_ID OB_TRANSPORT_ID_{transport}\n" in cfg


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
    assert "OB_USB_" not in gen_config("ch592", "generic-ch592-uart")


@pytest.mark.parametrize("chip,board,size,b_base", [
    # Two equal slots, floored to the erase block. ch570 generic spans all of
    # CodeFlash and halves exactly; the OpenDongle clamp to 0x3A000 keeps OBP
    # off the bond page and halves exactly too, at a smaller size. (Geometry is
    # transport-independent, so a single generic per chip suffices.)
    ("ch570", "generic-ch570-uart",  "0x0001D000", "0x0001F000"),
    ("ch570", "opendongle-ch570",    "0x0001C000", "0x0001E000"),
    ("ch592", "generic-ch592-uart",  "0x00037000", "0x00039000"),
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
         "build/generic/openboot_config.h"],
        capture_output=True, text=True)

    assert result.returncode != 0
    assert "is not usable on ch570" in result.stderr


def test_cpu_hz_override_lands_on_uart_builds():
    """OB_CPU_HZ pins the bootloader (and handoff) clock; the port's clock
    init #errors on unsupported values, so here we only prove the knob
    reaches the config header."""
    cfg_rel = "build/generic/openboot_config.h"
    subprocess.run(
        ["make", "--no-print-directory", "-C", str(FW),
         "CHIP=ch592", "TRANSPORT=uart", "OB_CPU_HZ=60000000", cfg_rel],
        check=True, capture_output=True, text=True)
    assert "#define OB_CPU_HZ 60000000\n" in (FW / cfg_rel).read_text()


def test_cpu_hz_override_is_refused_on_usb_builds():
    """USB requires the family PLL clock; the knob must not be able to
    silently produce a bootloader whose USB cannot enumerate."""
    result = subprocess.run(
        ["make", "--no-print-directory", "-C", str(FW),
         "CHIP=ch592", "TRANSPORT=usb", "OB_CPU_HZ=6400000",
         "build/generic/openboot_config.h"],
        capture_output=True, text=True)
    assert result.returncode != 0
    assert "only settable on UART builds" in result.stderr


def test_opencontroller_board_lands_its_knobs():
    """The keyboard module board is UART-transport with the PB12/PB13 remap;
    a regression in the board file or the Makefile include ordering would
    build a bootloader listening on the wrong pins (PA8/PA9) and brick the
    module's only wired update path."""
    cfg = gen_config("ch592", "opencontroller-ch592", transport="uart")
    assert "#define OB_TRANSPORT_ID OB_TRANSPORT_ID_UART\n" in cfg
    assert "#define OB_UART1_REMAP 1\n" in cfg
    assert "#define OB_UART1_ALT_PINS_HIZ 0\n" in cfg
    assert "#define OB_BOOT_IMAGE_CRC 1\n" in cfg
    assert "#define OB_IDLE_TIMEOUT_MS 10000\n" in cfg
    # No OB_APP_END clamp: the module's bond lives in DataFlash, so the full
    # app region stays available.
    assert "#define OB_FLASH_APP_END 0x00070000\n" in cfg


def test_mk65mx_wireless_board_lands_its_knobs():
    """The MK65 module uses UART1's default PA8/PA9 mapping.

    PB13 is CHWAKE on this board, so accidentally inheriting the original
    OpenController PB12/PB13 remap would both lose the host UART and drive the
    wake net with UART traffic.
    """
    cfg = gen_config("ch592", "mk65mx-wireless-ch592", transport="uart")
    assert "#define OB_TRANSPORT_ID OB_TRANSPORT_ID_UART\n" in cfg
    assert "#define OB_UART1_REMAP 0\n" in cfg
    assert "#define OB_UART1_ALT_PINS_HIZ 1\n" in cfg
    assert "#define OB_BOOT_IMAGE_CRC 1\n" in cfg
    assert "#define OB_IDLE_TIMEOUT_MS 10000\n" in cfg
    assert "#define OB_FLASH_APP_END 0x00070000\n" in cfg


@pytest.mark.parametrize("bad", ["2", "yes", "0 1"])
def test_uart_alt_pins_hiz_must_be_an_exact_boolean(bad):
    result = subprocess.run(
        ["make", "--no-print-directory", "-C", str(FW),
         "CHIP=ch592", "TRANSPORT=uart",
         f"OB_UART1_ALT_PINS_HIZ={bad}",
         "build/generic/openboot_config.h"],
        capture_output=True, text=True)

    assert result.returncode != 0
    assert "OB_UART1_ALT_PINS_HIZ must be 0 or 1" in result.stderr
