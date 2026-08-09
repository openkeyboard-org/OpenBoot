"""bl_version() parses boot_core.h's OB_BL_VERSION or dies.

The parse-or-die half is what keeps the golden HELLO vector honest: a
definition the generator cannot fully understand must be a hard error,
never a best-effort first token that disagrees with what the compiler
sees. These are the regression cases for that contract.
"""
import importlib.util
from pathlib import Path

import pytest

_SPEC = importlib.util.spec_from_file_location(
    "gen_protocol",
    Path(__file__).resolve().parents[2] / "protocol" / "gen_protocol.py",
)
gen = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(gen)


def _with_header(monkeypatch, tmp_path, text):
    header = tmp_path / "boot_core.h"
    header.write_text(text)
    monkeypatch.setattr(gen, "BOOT_CORE_H", header)


def test_plain_literal(monkeypatch, tmp_path):
    _with_header(monkeypatch, tmp_path, "#define OB_BL_VERSION 0x0102\n")
    assert gen.bl_version() == 0x0102


def test_literal_with_block_comment(monkeypatch, tmp_path):
    _with_header(
        monkeypatch, tmp_path, "#define OB_BL_VERSION 0x000B  /* v0.11 */\n"
    )
    assert gen.bl_version() == 0x000B


def test_literal_with_line_comment(monkeypatch, tmp_path):
    _with_header(
        monkeypatch, tmp_path, "#define OB_BL_VERSION 11  // v0.11\n"
    )
    assert gen.bl_version() == 11


def test_expression_is_rejected(monkeypatch, tmp_path):
    """`1 << 8` must die, not silently parse as 1."""
    _with_header(monkeypatch, tmp_path, "#define OB_BL_VERSION 1 << 8\n")
    with pytest.raises(SystemExit, match="not a single numeric literal"):
        gen.bl_version()


def test_trailing_tokens_are_rejected(monkeypatch, tmp_path):
    """`0x000B + 1` must die, not silently parse as 0x000B."""
    _with_header(monkeypatch, tmp_path, "#define OB_BL_VERSION 0x000B + 1\n")
    with pytest.raises(SystemExit, match="not a single numeric literal"):
        gen.bl_version()


def test_non_numeric_is_rejected(monkeypatch, tmp_path):
    _with_header(monkeypatch, tmp_path, "#define OB_BL_VERSION BL_WORD\n")
    with pytest.raises(SystemExit, match="not a plain numeric literal"):
        gen.bl_version()


def test_valueless_is_rejected(monkeypatch, tmp_path):
    _with_header(monkeypatch, tmp_path, "#define OB_BL_VERSION\n")
    with pytest.raises(SystemExit, match="not a plain numeric literal"):
        gen.bl_version()


def test_duplicate_is_rejected(monkeypatch, tmp_path):
    _with_header(
        monkeypatch,
        tmp_path,
        "#define OB_BL_VERSION 0x000B\n#define OB_BL_VERSION 0x000C\n",
    )
    with pytest.raises(SystemExit, match="defined twice"):
        gen.bl_version()


def test_missing_is_rejected(monkeypatch, tmp_path):
    _with_header(monkeypatch, tmp_path, "#define OB_SOMETHING_ELSE 1\n")
    with pytest.raises(SystemExit, match="no '#define OB_BL_VERSION"):
        gen.bl_version()


def test_real_header_parses():
    """The checked-in boot_core.h must satisfy the contract as-is."""
    assert gen.bl_version() == gen.bl_version()  # parses without dying
    assert 0 <= gen.bl_version() <= 0xFFFF  # fits the u16 wire field
