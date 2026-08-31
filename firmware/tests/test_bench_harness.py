"""Static checks on the hardware bench harness in tests/bench/.

Nothing here drives hardware — the harness needs a bench with two WCH-LinkE
probes to run. These checks exist because the harness once shipped compiling a
source file that had been renamed out from under it. It was broken for
everyone who tried to run it, and no suite noticed, because tests/bench/ is
not reachable from `make test` and never will be.

So this pins the couplings a rename or an argument reshuffle would break,
which is the whole class of failure that got through.
"""
import ast
import re
import subprocess
from pathlib import Path
from types import SimpleNamespace

import pytest

BENCH = Path(__file__).resolve().parent / "bench"
BUILDER = BENCH / "build_witness.sh"
HARNESS = BENCH / "ab_bench.py"


def test_harness_parses():
    """Syntax only: importing them would require pyserial/hidapi, which a
    checkout has no reason to have installed."""
    sources = sorted(BENCH.glob("*.py"))
    assert sources, f"no bench sources under {BENCH}; this check went blind"
    for py in sources:
        ast.parse(py.read_text(), str(py))


def test_builder_is_valid_shell():
    subprocess.run(["bash", "-n", str(BUILDER)], check=True)


def test_every_source_the_builder_compiles_exists():
    """The escape this file exists for: the builder named marker.S after the
    file had been committed as witness.S."""
    named = set(re.findall(r"([A-Za-z0-9_]+\.S)\b", BUILDER.read_text()))
    assert named, "builder no longer names a source file; this check went blind"
    for src in named:
        assert (BENCH / src).is_file(), f"build_witness.sh compiles missing {src}"


def test_builder_and_witness_agree_on_macros():
    """Every -D the builder passes must be one the source actually uses, and
    every macro the source reads must be one the builder passes. Either half
    drifting silently produces an image that assembles and misbehaves."""
    passed = set(re.findall(r"-D([A-Z_]+)=", BUILDER.read_text()))
    # Comments are prose and full of capitalised words ("SRAM", "CLEARS"),
    # so strip them before looking for macro uses.
    code = re.sub(r"/\*.*?\*/", " ", (BENCH / "witness.S").read_text(), flags=re.S)
    used = set(re.findall(r"\b([A-Z][A-Z_]{3,})\b", code))
    assert passed, "builder passes no -D macros; this check went blind"
    assert passed <= used, f"builder passes macros the source ignores: {passed - used}"
    assert used <= passed, f"source reads macros the builder never passes: {used - passed}"


def test_readme_examples_match_the_builder_arity():
    """The README is the only usage documentation, and the builder takes six
    positional arguments in a specific order."""
    want = int(re.search(r"\$# -ne (\d+)", BUILDER.read_text()).group(1))
    examples = [ln for ln in (BENCH / "README.md").read_text().splitlines()
                if "build_witness.sh" in ln and ln.strip().startswith("./")]
    assert examples, "README shows no invocation to check"
    for line in examples:
        got = len(line.split()) - 1
        assert got == want, f"README example passes {got} args, builder wants {want}: {line}"


@pytest.mark.parametrize("chip", ["ch570", "ch572", "ch592"])
def test_chip_config_carries_what_the_scenarios_read(chip):
    """The scenarios index the per-chip config by key; a missing one fails
    only once a bench run reaches that line, minutes in."""
    tree = ast.parse(HARNESS.read_text())
    chips = next(n.value for n in ast.walk(tree)
                 if isinstance(n, ast.Assign)
                 and any(getattr(t, "id", None) == "CHIPS" for t in n.targets))
    entry = dict(zip([k.value for k in chips.keys], chips.values))[chip]
    keys = {kw.arg for kw in entry.keywords}
    assert {"serial", "port", "boot", "slot_b", "cap", "bootreq"} <= keys


# --- mc() behaviour ---------------------------------------------------------
# The static checks above cannot see mc()'s argv assembly, which is subtle and
# has broken silently before: minichlink derives skip_startup from argv[1], so
# the ACTION must lead and -l <serial> must trail; -t/-3 must become -kt/-k3 so
# power control needs no live chip; and a zero exit that never reached the chip
# must still raise. ab_bench imports without pyserial now (lazy), so these can
# run in CI, unlike the hardware scenarios.

def _ab_bench():
    import importlib
    import sys

    sys.path.insert(0, str(BENCH))
    try:
        return importlib.import_module("ab_bench")
    finally:
        sys.path.pop(0)


class _FakeRun:
    """Records the argv mc() built; returns a canned minichlink result."""

    def __init__(self, rc=0, out="", err=""):
        self.rc, self.out, self.err = rc, out, err
        self.calls = []

    def __call__(self, cmd, t=90):
        self.calls.append(list(cmd))
        return SimpleNamespace(returncode=self.rc, stdout=self.out, stderr=self.err)


def test_mc_puts_the_action_first_and_the_serial_last(monkeypatch):
    ab = _ab_bench()
    fake = _FakeRun()
    monkeypatch.setattr(ab, "run", fake)
    ab.mc({"serial": "SER1"}, "-r", "+", "0x0", "16")
    assert fake.calls[-1] == [ab.MC, "-r", "+", "0x0", "16", "-l", "SER1"]


def test_mc_maps_power_control_through_k(monkeypatch):
    ab = _ab_bench()
    fake = _FakeRun()
    monkeypatch.setattr(ab, "run", fake)
    ab.mc({"serial": "S"}, "-t")
    assert fake.calls[-1][1] == "-kt", "-t must become -kt (power needs no live chip)"
    ab.mc({"serial": "S"}, "-3")
    assert fake.calls[-1][1] == "-k3"


def test_mc_refuses_an_empty_action():
    ab = _ab_bench()
    with pytest.raises(ValueError):
        ab.mc({"serial": "S"})


def test_mc_flags_a_zero_exit_that_never_reached_the_chip(monkeypatch):
    ab = _ab_bench()
    monkeypatch.setattr(
        ab, "run", _FakeRun(rc=0, err="link error, nothing connected to linker")
    )
    with pytest.raises(RuntimeError):
        ab.mc({"serial": "S"}, "-A")


def test_mc_raises_on_a_nonzero_exit(monkeypatch):
    ab = _ab_bench()
    monkeypatch.setattr(ab, "run", _FakeRun(rc=1, err="boom"))
    with pytest.raises(RuntimeError):
        ab.mc({"serial": "S"}, "-A")
