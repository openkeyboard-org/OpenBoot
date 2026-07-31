#!/usr/bin/env python3
"""Generate everything that must agree with protocol/openboot_protocol.h.

The C header stays the hand-written source of truth — it carries the prose
that defines the protocol and the boot-record struct. This script parses its
`#define OB_*` numerics and emits:

  1. tools/src/proto/consts.rs        Rust constants (DO NOT EDIT)
  2. firmware/tests/ob_consts.py      the same constants for the pytest suite
  3. protocol/golden_frames.txt       normative wire vectors, built FROM the
                                      parsed constants (no inline literals)

Run from anywhere:  python3 protocol/gen_protocol.py
Check without writing (used by `make -C firmware test`):
                    python3 protocol/gen_protocol.py --check

Golden vectors are LOGICAL frames (no UART SOF prefix, no USB report
padding); format is one `name: hexbytes` pair per line, '#' starts a
comment.
"""
import sys
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
HEADER = ROOT / "protocol" / "openboot_protocol.h"
CONSTS_RS = ROOT / "tools" / "src" / "proto" / "consts.rs"
OB_CONSTS_PY = ROOT / "firmware" / "tests" / "ob_consts.py"
GOLDEN = ROOT / "protocol" / "golden_frames.txt"

assert zlib.crc32(b"123456789") == 0xCBF43926, "CRC-32/ISO-HDLC sanity"

# Rust type per constant. Deliberately explicit rather than inferred: the
# types are an API decision (indices and lengths are usize, wire fields are
# sized), and an unmapped name defaults to u8 only because that is what the
# overwhelming majority are — see rust_type().
RUST_TYPES = {
    "usize": {
        "OB_FRAME_HDR_LEN",
        "OB_FRAME_CRC_LEN",
        "OB_FRAME_OVERHEAD",
        "OB_MAX_PAYLOAD",
        "OB_MAX_FRAME",
        "OB_MAX_WRITE_DATA",
        "OB_HELLO_REQ_LEN",
        "OB_HELLO_RESP_LEN",
        "OB_BOOT_RECORD_SIZE",
    },
    "u32": {
        "OB_UART_BAUD",
        "OB_HELLO_MAGIC",
        "OB_FEAT_READ",
        "OB_FEAT_CRC_LIVE",
        "OB_RECORD_MAGIC",
        "OB_BOOTREQ_MAGIC",
        "OB_BOOTREQ_ADDR_CH57X",
        "OB_BOOTREQ_ADDR_CH59X",
        "OB_APP_BASE",
    },
    "u64": {"OB_UART_INTERBYTE_MS"},
}


# Everything not named above is a byte-wide wire field (opcodes, statuses,
# details, families). Listing them makes "what type is this constant?" an
# explicit decision at the moment a constant is added, rather than a default
# that is right until the day it is not.
RUST_TYPES["u8"] = None                       # filled in by check_types()


def rust_type(name: str) -> str:
    for ty, names in RUST_TYPES.items():
        if names is not None and name in names:
            return ty
    return "u8"


def check_types(consts: dict) -> None:
    """A u8-by-default constant that does not fit in a u8 would generate Rust
    that fails to compile; one that fits but is semantically wider would
    compile and be wrong. Catch both here, where the fix is one line."""
    for name, value in consts.items():
        if rust_type(name) == "u8" and value > 0xFF:
            raise SystemExit(
                f"{name} = 0x{value:X} does not fit the default u8; add it to "
                "RUST_TYPES in this script with the type it should have"
            )


def parse_numeric(token: str):
    """A C numeric literal: 0x-hex or decimal, one optional u/U suffix."""
    t = token.removesuffix("u").removesuffix("U")
    try:
        return int(t, 16) if t.lower().startswith("0x") else int(t, 10)
    except ValueError:
        return None


def split_define(line: str):
    """Tokens of a `#define` directive, or None if the line is not one.

    Tolerates the whitespace C allows between `#` and `define`, and after
    it, so `#  define OB_X 1` cannot slip past unnoticed.
    """
    stripped = line.lstrip()
    if not stripped.startswith("#"):
        return None
    after_hash = stripped[1:].lstrip()
    if not after_hash.startswith("define"):
        return None
    rest = after_hash[len("define") :]
    if rest[:1] not in (" ", "\t"):          # `#defineOB_X` is not a define
        return None
    tokens = rest.split()
    return tokens or None


def parse_header(path: Path) -> dict:
    """Every `#define OB_<name> <numeric>`, in header order.

    A non-numeric, valueless, function-like or duplicate OB_* define is a
    hard error: silently skipping one would let a new protocol constant
    exist in C with no counterpart anywhere else.
    """
    out = {}
    for line in path.read_text().splitlines():
        tokens = split_define(line)
        if tokens is None:
            continue
        name = tokens[0]
        if not name.startswith("OB_"):
            continue
        if "(" in name:
            raise SystemExit(f"{path}: function-like macro {name} is not supported")
        if len(tokens) < 2:
            raise SystemExit(f"{path}: {name} has no value")
        value = parse_numeric(tokens[1])
        if value is None:
            raise SystemExit(
                f"{path}: {name} = {tokens[1]!r} is not a plain numeric literal; "
                "the generator only understands 0x-hex and decimal"
            )
        # Anything past the value must be a comment. Without this an
        # expression like `1 << 8` would parse as its first token and the
        # generated 1 would silently disagree with the 256 the compiler sees.
        if len(tokens) > 2 and not tokens[2].startswith(("/*", "//")):
            raise SystemExit(
                f"{path}: {name} = {' '.join(tokens[1:])!r} is not a single "
                "numeric literal; the generator cannot evaluate expressions, "
                "so write the computed value"
            )
        if name in out:
            raise SystemExit(f"{path}: {name} defined twice")
        out[name] = value
    if not out:
        raise SystemExit(f"{path}: no OB_* defines found")
    return out


BANNER_RS = """\
//! Protocol constants — GENERATED, DO NOT EDIT.
//!
//! Source of truth: `protocol/openboot_protocol.h`.
//! Regenerate with `python3 protocol/gen_protocol.py`; the
//! `generated_consts_match_header` test fails if this file and the header
//! ever disagree, so a hand edit is caught rather than trusted.

// Every header constant is emitted whether or not the tool uses it yet.
#![allow(dead_code)]

"""

BANNER_PY = '''\
"""Protocol constants — GENERATED, DO NOT EDIT.

Source of truth: protocol/openboot_protocol.h.
Regenerate with `python3 protocol/gen_protocol.py`; `make -C firmware test`
runs `--check`, so a stale copy fails the suite.
"""
'''


def emit_consts_rs(c: dict) -> str:
    lines = [BANNER_RS]
    for name, value in c.items():
        lines.append(f"pub const {name}: {rust_type(name)} = 0x{value:X};\n")
    return "".join(lines)


def emit_consts_py(c: dict) -> str:
    lines = [BANNER_PY]
    for name, value in c.items():
        lines.append(f"{name} = 0x{value:X}\n")
    return "".join(lines)


def emit_golden(c: dict) -> str:
    """Wire vectors built from the parsed constants: no opcode or status
    literal appears in this function."""

    # This encoder hardcodes the frame SHAPE: a 4-byte header
    # (cmd, seq, len, flags) and a 4-byte trailing CRC. It cannot derive an
    # alternative layout from the constants, so it asserts that the header
    # still describes the shape it builds — otherwise a geometry change
    # would regenerate byte-identical vectors and every check would pass.
    assert c["OB_FRAME_HDR_LEN"] == 4, "frame header is no longer 4 bytes"
    assert c["OB_FRAME_CRC_LEN"] == 4, "frame CRC is no longer 4 bytes"
    assert c["OB_FRAME_OVERHEAD"] == c["OB_FRAME_HDR_LEN"] + c["OB_FRAME_CRC_LEN"]

    def frame(cmd: int, seq: int, payload: bytes = b"", flags: int = 0) -> bytes:
        assert 0 <= len(payload) <= c["OB_MAX_PAYLOAD"]
        body = bytes([cmd, seq, len(payload), flags]) + payload
        return body + zlib.crc32(body).to_bytes(4, "little")

    def u32(v: int) -> bytes:
        return v.to_bytes(4, "little")

    def u16(v: int) -> bytes:
        return v.to_bytes(2, "little")

    resp = c["OB_CMD_RESP_BIT"]
    ok = c["OB_OK"]

    # A CH592A over USB: the reference HELLO response every implementation
    # must reproduce byte for byte.
    hello_resp = (
        bytes([ok, c["OB_PROTO_MAJOR"], c["OB_PROTO_MINOR"], 9])
        + u16(0x000A)  # bl_version v0.10
        + bytes([c["OB_FAMILY_CH592"], c["OB_TRANSPORT_ID_USB"]])
        + u32(c["OB_APP_BASE"])
        + u32(0x00070000)  # app region (CH592)
        + u32(4096)
        + u16(256)
        + bytes([4, c["OB_MAX_WRITE_DATA"]])
        + u32(c["OB_FEAT_CRC_LIVE"])
        + (0x0123456789ABCDEF).to_bytes(8, "little")  # uid
    )
    assert len(hello_resp) == c["OB_HELLO_RESP_LEN"]

    magic = c["OB_HELLO_MAGIC"].to_bytes(4, "little")
    proto = bytes([c["OB_PROTO_MAJOR"], c["OB_PROTO_MINOR"]])
    assert len(magic) + len(proto) == c["OB_HELLO_REQ_LEN"]
    base = c["OB_APP_BASE"]
    vectors = [
        ("crc_check", zlib.crc32(b"123456789").to_bytes(4, "little")),
        ("hello_req", frame(c["OB_CMD_HELLO"], 0x00, magic + proto)),
        ("hello_resp_ch592_usb", frame(c["OB_CMD_HELLO"] | resp, 0x00, hello_resp)),
        ("erase_req", frame(c["OB_CMD_ERASE"], 0x01, u32(base) + u32(0x1000))),
        ("erase_ok", frame(c["OB_CMD_ERASE"] | resp, 0x01, bytes([ok]))),
        (
            "write_req",
            frame(
                c["OB_CMD_WRITE"], 0x02, u32(base) + bytes.fromhex("deadbeefcafebabe")
            ),
        ),
        ("write_ok", frame(c["OB_CMD_WRITE"] | resp, 0x02, bytes([ok]))),
        (
            "write_err_not_erased",
            frame(
                c["OB_CMD_WRITE"] | resp,
                0x02,
                bytes([c["OB_E_NOT_ERASED"], c["OB_DET_NONE"]]),
            ),
        ),
        ("crc_req", frame(c["OB_CMD_CRC"], 0x03, u32(base) + u32(0x2000))),
        ("crc_ok", frame(c["OB_CMD_CRC"] | resp, 0x03, bytes([ok]) + u32(0xCBF43926))),
        ("commit_req", frame(c["OB_CMD_COMMIT"], 0x04, u32(0x9C40) + u32(0x12345678))),
        ("commit_ok", frame(c["OB_CMD_COMMIT"] | resp, 0x04, bytes([ok]))),
        (
            "commit_err_nonseq",
            frame(
                c["OB_CMD_COMMIT"] | resp,
                0x04,
                bytes([c["OB_E_VERIFY"], c["OB_DET_VERIFY_NONSEQ"]]),
            ),
        ),
        ("boot_req", frame(c["OB_CMD_BOOT"], 0x05, bytes([c["OB_BOOT_APP"]]))),
        ("boot_ok", frame(c["OB_CMD_BOOT"] | resp, 0x05, bytes([ok]))),
        (
            "frame_err",
            frame(c["OB_CMD_FRAME_ERR"], 0x09, bytes([c["OB_E_CRC"], c["OB_DET_NONE"]])),
        ),
        # WRITE with the maximum payload (bytes 0x00..), addr 0x3000
        (
            "write_req_max",
            frame(
                c["OB_CMD_WRITE"], 0x06, u32(0x3000) + bytes(range(c["OB_MAX_WRITE_DATA"]))
            ),
        ),
        # empty-payload frame (not a legal command, but a legal encoding)
        ("min_frame", frame(0x7F, 0xAA)),
    ]
    lines = ["# OBP v0.1 golden vectors — generated by gen_protocol.py; do not edit.\n"]
    lines += [f"{name}: {data.hex()}\n" for name, data in vectors]
    return "".join(lines)


def main() -> int:
    check = "--check" in sys.argv[1:]
    consts = parse_header(HEADER)
    check_types(consts)
    outputs = {
        CONSTS_RS: emit_consts_rs(consts),
        OB_CONSTS_PY: emit_consts_py(consts),
        GOLDEN: emit_golden(consts),
    }
    stale = []
    for path, text in outputs.items():
        current = path.read_text() if path.exists() else None
        if current == text:
            continue
        if check:
            stale.append(path)
        else:
            path.write_text(text)
    if check:
        if stale:
            for path in stale:
                print(f"STALE: {path.relative_to(ROOT)}", file=sys.stderr)
            print("run: python3 protocol/gen_protocol.py", file=sys.stderr)
            return 1
        print(f"generated files are up to date ({len(consts)} constants)")
        return 0
    print(f"wrote {len(outputs)} files from {len(consts)} header constants")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
