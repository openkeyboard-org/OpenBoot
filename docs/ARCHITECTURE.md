# OpenBoot firmware architecture

OpenBoot combines a portable protocol core with one chip-family port and one
transport. The wire format is specified in [PROTOCOL.md](PROTOCOL.md); the
firmware interfaces are defined by
[`openboot_port.h`](../firmware/core/openboot_port.h) and
[`openboot_transport.h`](../firmware/core/openboot_transport.h).

## Repository structure

| Path | Purpose |
|---|---|
| `Makefile` | Orchestrates repository-wide build, test, check, and clean workflows |
| `protocol/` | Shared protocol constants, generator, and golden frames |
| `firmware/core/` | Portable state machine, boot decision, and CRC |
| `firmware/ports/` | CH57x and CH59x clocks, flash, records, identity, and reset |
| `firmware/transports/` | USB HID and UART framing |
| `firmware/boards/` | Board-specific pins and policy |
| `firmware/app/` | Application companion API |
| `firmware/tests/` | Host-native tests of the real core sources |
| `tools/` | Rust host CLI |
| `third_party/openwch/` | Pinned WCH headers and `libISP` archives |

`protocol/openboot_protocol.h` is the only hand-written source of protocol
numbers. `protocol/gen_protocol.py` generates the Rust and Python mirrors and
the golden frames. Firmware and host tests check that generated files remain in
sync.

## Component boundaries

Each image links exactly one implementation on either side of the core:

```text
                    portable core
          framing, commands, session, safety
                 /                  \
        chip-family port          transport
       CH57x or CH59x          USB HID or UART
```

The boundaries are deliberately narrow:

- The core validates frame CRCs, lengths, opcodes, session state, ranges,
  alignment, and the erased-block bitmap.
- Transports only receive and send candidate logical frames. UART adds the SOF
  parser; USB maps one frame to one 64-byte HID report.
- Ports own all special-function-register access, flash operations, boot-record
  storage, chip identity, delays, jumps, and resets.
- The firmware is polled. It enables no interrupts, timers, or DMA; the main
  loop uses `OB_POLL_INTERVAL_US` as its time base.

There are no vtables or dynamic allocation. Selecting a port and transport at
link time keeps the interfaces direct and the image small.

## Family differences

| | CH570 / CH572 | CH591 | CH592 |
|---|---|---|---|
| App region | `[0x2000, 0x3B000)` (228 KiB) | `[0x2000, 0x30000)` (184 KiB) | `[0x2000, 0x70000)` (440 KiB) |
| RAM | 12 KiB | 26 KiB | 26 KiB |
| Boot record | Code flash `0x3B000` | DataFlash `0x7000` | DataFlash `0x7000` |
| UART pins | PA2 RX, PA3 TX | PA8 RX, PA9 TX by default | PA8 RX, PA9 TX by default |
| USB clock | 100 MHz | 60 MHz | 60 MHz |
| Live XIP CRC | No (`OB_FEAT_CRC_LIVE` clear) | Yes | Yes |

All builds use 4 KiB erase blocks, 256-byte write pages, 4-byte write
alignment, and an application base of `0x2000`. UART builds stay at the
6.4 MHz reset clock. CH59x can remap UART1 to PB12/PB13 through a board
setting.

CH57x USB attachment clears `RB_PIN_DEBUG_EN` because PA0/PA1 are shared with
SWD. See the recovery procedure in the
[firmware README](../firmware/README.md#ch570ch572-usb-recovery) before
flashing those images.

No shipped board defines an OpenBoot stay-in-bootloader strap. Products may
set `OB_BOOT_PIN_MASK`, but mask-ROM ISP remains the recovery mechanism.

## Flash and update safety

All flash access goes through the WCH ROM entry point exposed by `libISP572.a`
or `libISP592.a`. The port wrappers do not range-check because the shared core
does so before every operation.

The core enforces four update invariants:

1. ERASE, WRITE, and CRC can address only the application region.
2. WRITE is accepted only for blocks erased in the current session.
3. The boot record is invalidated before the first mutation.
4. COMMIT writes a new record only after the image CRC is verified.

Consequently, an interrupted update returns to the bootloader rather than
starting a partial image. OBP intentionally has no bootloader self-update
command; bootloader recovery uses SWD or mask-ROM ISP.

CH57x has an additional constraint: XIP reads may return stale data after a
flash-controller write in the same power cycle. WRITE uses controller verify,
and COMMIT uses the sequential stream CRC for an in-session update. BOOT resets
the chip before launching the application so instruction fetches are coherent.

## Startup and memory layout

Each family has a small vectorless startup file that initializes `sp` and
`gp`, copies `.highcode` and `.data`, clears `.bss`, installs a trap parking
loop, and calls `main`. Interrupts remain disabled.

One linker script serves all variants. The build generates the family-specific
RAM geometry and boot-request address. It also:

- limits the bootloader flash region to 8 KiB;
- places `libISP` and required clock code in RAM-backed `.highcode`;
- reserves and checks a 2 KiB stack; and
- excludes the top 16 bytes of RAM for the application-to-bootloader request.

Applications must use the matching layout described in
[`firmware/app/README.md`](../firmware/app/README.md).

## Build and size controls

The firmware Makefile builds this matrix:

```text
CHIP={ch570,ch572,ch591,ch592} x TRANSPORT={usb,uart}
```

Each configuration has its own build directory. A non-default `BOARD=` adds
the board name to that directory, preventing object reuse across variants.
The generated `openboot_config.h` is force-included in every source file.

Every image must fit in 8192 bytes. The linker region and post-objcopy size
check enforce the limit; `make check` from the repository root displays the
current size of all eight images, applies the board-policy check, and runs both
test suites. Exact sizes belong in that generated report rather than in
documentation, where they quickly become stale.

## Verification

`make test` from the repository root compiles the production `core/*.c` files
into `firmware/build/host/` against a simulated flash port, drives them with
pytest, and runs the Rust tests. The mock models erase/write rules, bounds,
CH57x stale-XIP behavior, and power loss during the update sequence.

The suite covers framing, address and alignment errors, write-before-erase,
sequential COMMIT rules, retry behavior, boot decisions, and power-cut
recovery. The Rust tests consume the same golden frames and cross-check their
protocol constant mirror. Hardware-specific validation remains the bring-up
checklist in the [firmware README](../firmware/README.md#hardware-bring-up).

## Non-goals

- SDK driver sources; only register headers and `libISP` are used
- interrupts, timers, DMA, or dynamic allocation
- dual-transport images
- bootloader self-update
- authentication, signing, encryption, or multi-image management
