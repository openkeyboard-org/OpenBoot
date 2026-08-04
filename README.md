# OpenBoot

OpenBoot is an 8 KiB IAP bootloader for WCH RISC-V microcontrollers. It uses
one protocol over USB HID or UART and includes a cross-platform Rust flashing
tool.

| Chip | App region | RAM | Transports |
|---|---|---|---|
| CH570 / CH572 | `[0x2000, 0x3B000)` | 12 KiB | USB, UART |
| CH591 | `[0x2000, 0x30000)` | 26 KiB | USB, UART |
| CH592 | `[0x2000, 0x70000)` | 26 KiB | USB, UART |

The bootloader occupies `[0x0000, 0x2000)`; applications are linked at
`0x2000`. Each chip and transport combination produces a separate image.

OpenBoot prevents protocol requests from modifying the bootloader, requires a
block to be erased before it can be written, invalidates the previous boot
record before an update, and makes the new image bootable only after COMMIT
verifies its CRC. The device reports its flash geometry and identity through
HELLO, so the host tool needs no per-chip database.

## Repository layout

| Path | Contents |
|---|---|
| `Makefile` | Repository-wide build, test, check, and clean targets |
| `protocol/` | Shared protocol constants and golden frames |
| `firmware/` | Bootloader, ports, transports, boards, and tests |
| `tools/` | `openboot` host CLI |
| `docs/` | [Protocol](docs/PROTOCOL.md), [architecture](docs/ARCHITECTURE.md), [A/B update design](docs/AB-UPDATE.md) |
| `third_party/openwch/` | Pinned WCH SDK submodules |

## Build

Requirements: GNU Make 4.3 or newer, the MounRiver "RISC-V Embedded GCC 12"
toolchain, Python 3, and Rust.

```sh
git submodule update --init
export MRS_TOOLCHAIN=/path/to/toolchain/bin
make          # all firmware images and the release host tool
make test
make check    # tests plus firmware size and board-policy checks
```

Use `make firmware` or `make tool` to build only one component. The firmware
and tool directories retain their own Make and Cargo entry points for targeted
development.

See the [firmware guide](firmware/README.md) for toolchain setup, flashing,
CH57x USB recovery, and hardware bring-up. See the
[application guide](firmware/app/README.md) for linker and integration
requirements, and the [CLI guide](tools/README.md) for host usage.

OpenBoot is licensed under Apache-2.0.
