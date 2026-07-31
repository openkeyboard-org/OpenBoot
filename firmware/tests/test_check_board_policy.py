"""Tests for strict default-board artifact policy discovery."""

import sys

import pytest

import check_board_policy as policy


def run_policy(monkeypatch, tmp_path, artifact: str) -> int:
    build = tmp_path / "build" / "ch570-usb"
    build.mkdir(parents=True)
    (build / artifact).touch()
    monkeypatch.setattr(policy, "symbol_size", lambda *_args: 4)
    monkeypatch.setattr(
        sys,
        "argv",
        ["check_board_policy.py", "--nm", "unused-nm", "--build", str(tmp_path / "build")],
    )
    return policy.main()


def test_valid_artifact_is_checked(monkeypatch, tmp_path, capsys):
    assert run_policy(monkeypatch, tmp_path, "openboot-ch570-usb.elf") == 0
    assert "board policy ok: 1 images" in capsys.readouterr().out


@pytest.mark.parametrize(
    "artifact",
    (
        "openboot-ch570-usb-backup.elf",
        "openboot-ch570-spi.elf",
        "openboot-unrecognised.elf",
    ),
)
def test_unexpected_artifact_name_fails(monkeypatch, tmp_path, capsys, artifact):
    assert run_policy(monkeypatch, tmp_path, artifact) == 1
    assert "unexpected OpenBoot artifact name" in capsys.readouterr().err


def test_unknown_chip_fails(monkeypatch, tmp_path, capsys):
    assert run_policy(monkeypatch, tmp_path, "openboot-ch999-usb.elf") == 1
    assert "unknown chip ch999" in capsys.readouterr().err
