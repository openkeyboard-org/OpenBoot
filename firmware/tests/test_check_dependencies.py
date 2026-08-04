"""Focused tests for the cross-toolchain dependency checks."""

from pathlib import Path

import pytest

import check_dependencies as deps


def fake_toolchain(tmp_path: Path, major: int = 12, prefix: str = None) -> Path:
    """A directory that looks like a MounRiver bin/ to the checker.

    prefix defaults to the first supported one; pass another to exercise the
    GCC15 rename (riscv-wch-elf-* -> riscv32-wch-elf-*)."""
    if prefix is None:
        prefix = deps.TOOL_PREFIXES[0]
    for name in deps.REQUIRED_TOOLS:
        tool = tmp_path / f"{prefix}{name}"
        if name == "gcc":
            tool.write_text(
                "#!/bin/sh\n"
                f"printf '%s\\n' '{prefix}gcc (Fake) {major}.2.0'\n"
            )
        else:
            tool.write_text("#!/bin/sh\nexit 0\n")
        tool.chmod(0o755)
    return tmp_path


def test_different_fingerprint_warns(tmp_path, capsys):
    deps.validate_toolchain(str(fake_toolchain(tmp_path)))

    assert "dependency check warning: compiler SHA-256" in capsys.readouterr().err


@pytest.mark.parametrize("major", deps.SUPPORTED_GCC_MAJORS)
def test_supported_gcc_major_is_not_reported(tmp_path, capsys, major):
    deps.validate_toolchain(str(fake_toolchain(tmp_path, major=major)))

    assert "not a validated compiler" not in capsys.readouterr().err


def test_unvalidated_gcc_major_warns_and_proceeds(tmp_path, capsys):
    """A compiler we have not qualified must not block the build."""
    deps.validate_toolchain(str(fake_toolchain(tmp_path, major=13)))

    assert "not a validated compiler" in capsys.readouterr().err


@pytest.mark.parametrize("prefix", deps.TOOL_PREFIXES)
def test_either_tool_prefix_is_accepted(tmp_path, prefix):
    """GCC15 renamed riscv-wch-elf-* to riscv32-wch-elf-*; both must work."""
    toolchain = fake_toolchain(tmp_path, prefix=prefix)

    assert deps.detect_tool_prefix(toolchain) == prefix
    deps.validate_toolchain(str(toolchain))


def test_missing_required_tool_still_fails(tmp_path):
    toolchain = fake_toolchain(tmp_path)
    (toolchain / f"{deps.TOOL_PREFIXES[0]}size").unlink()

    with pytest.raises(RuntimeError, match="missing or not executable"):
        deps.validate_toolchain(str(toolchain))


def test_directory_without_any_compiler_fails(tmp_path):
    with pytest.raises(RuntimeError, match="no MounRiver compiler"):
        deps.validate_toolchain(str(tmp_path))


def fake_git(head: str, status: str = ""):
    return lambda tree, *args: head if args[0] == "rev-parse" else status


def test_expect_revision_accepts_a_clean_matching_checkout(monkeypatch):
    monkeypatch.setattr(deps, "run_git", fake_git("cafe"))

    deps.validate_self("cafe")


def test_expect_revision_rejects_a_different_head(monkeypatch):
    monkeypatch.setattr(deps, "run_git", fake_git("deadbeef"))

    with pytest.raises(RuntimeError, match="expected cafe"):
        deps.validate_self("cafe")


def test_expect_revision_rejects_a_dirty_checkout(monkeypatch):
    """Matching HEAD is not enough: local edits change the built bytes."""
    monkeypatch.setattr(deps, "run_git", fake_git("cafe", " M firmware/Makefile"))

    with pytest.raises(RuntimeError, match="dirty"):
        deps.validate_self("cafe")
