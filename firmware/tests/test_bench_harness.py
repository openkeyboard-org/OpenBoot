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
