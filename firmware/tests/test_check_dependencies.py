"""Focused tests for the cross-toolchain dependency checks."""

from pathlib import Path

import pytest

import check_dependencies as deps


def fake_toolchain(tmp_path: Path, major: int = 12) -> Path:
    for name in deps.REQUIRED_TOOLS:
        tool = tmp_path / name
        if name == "riscv-wch-elf-gcc":
            tool.write_text(
                "#!/bin/sh\n"
                f"printf '%s\\n' 'riscv-wch-elf-gcc (Fake) {major}.2.0'\n"
            )
        else:
            tool.write_text("#!/bin/sh\nexit 0\n")
        tool.chmod(0o755)
    return tmp_path


def test_different_gcc12_fingerprint_warns(tmp_path, capsys):
    deps.validate_toolchain(str(fake_toolchain(tmp_path)))

    assert "dependency check warning: compiler SHA-256" in capsys.readouterr().err


def test_wrong_gcc_major_still_fails(tmp_path):
    toolchain = fake_toolchain(tmp_path, major=13)

    with pytest.raises(RuntimeError, match="not the required GCC 12.x"):
        deps.validate_toolchain(str(toolchain))


def test_missing_required_tool_still_fails(tmp_path):
    toolchain = fake_toolchain(tmp_path)
    (toolchain / "riscv-wch-elf-size").unlink()

    with pytest.raises(RuntimeError, match="missing or not executable"):
        deps.validate_toolchain(str(toolchain))
