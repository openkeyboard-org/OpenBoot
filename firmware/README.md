# OpenBoot firmware

The firmware builds one bootloader image for each supported chip and
transport. Each image occupies flash `[0x0000, 0x2000)`; the application starts
at `0x2000`.

> [!WARNING]
> CH570/CH572 USB builds disable SWD because USB shares PA0/PA1 with the debug
> interface. Read [CH570/CH572 USB recovery](#ch570ch572-usb-recovery) before
> flashing one. UART builds leave SWD enabled and are the safer bring-up path.

See the [protocol specification](../docs/PROTOCOL.md),
[architecture](../docs/ARCHITECTURE.md), and
[application integration guide](app/README.md) for related details.

## Prerequisites

| Tool | Requirement |
|---|---|
| GNU Make | 4.3 or newer |
| MounRiver GCC | "RISC-V Embedded GCC" 12 or 15 |
| Python | Python 3 and pytest for host tests |
| WCH SDK files | Initialized repository submodules |

Initialize dependencies from the repository root:

```sh
git submodule update --init
```

Point `MRS_TOOLCHAIN` at the directory containing `riscv-wch-elf-gcc`:

```sh
export MRS_TOOLCHAIN=/path/to/toolchain/bin
```

If the toolchain path causes Make quoting problems, use a space-free symlink.

GCC 12 and GCC 15 both build the whole matrix; GCC 15 renamed the tools from
`riscv-wch-elf-*` to `riscv32-wch-elf-*` and the build detects either. The two
emit slightly different code (−12 to +80 bytes per image here), so an image is
only byte-reproducible against the compiler that built it.

`make check-deps` treats the pinned SDK revisions as hard failures and the
compiler as advisory: an unvalidated major and a SHA-256 that differs from the
reference-build fingerprint both warn and build on. GCC 15's linker also warns
once per image about the RWX `LOAD` segment — accurate for a no-MMU image, and
GCC 12's linker rejects the option that would silence it.

## Build

The matrix is `CHIP={ch570,ch572,ch591,ch592}` by
`TRANSPORT={usb,uart}`. Outputs are written to
`build/<chip>-<transport>/`; non-default boards add `+<board>` to the
directory name.

```sh
make CHIP=ch592 TRANSPORT=usb   # one image
make all                        # all eight images
make matrix-report              # build, size, and board-policy checks
make test                       # host-native core tests
```

Every binary is limited to 8192 bytes by the linker and a post-build check.

## Packaging OpenBoot in another project

Ask where the image lands rather than reconstructing the directory name, which
is a build-system detail:

```sh
make --no-print-directory CHIP=ch592 TRANSPORT=usb BOARD=myboard print-image-path
```

It prints one absolute path and nothing else, builds nothing, and needs no
`MRS_TOOLCHAIN`. The `.elf`, `.hex`, and `.map` share the directory and stem.
`--no-print-directory` matters when the result is captured: Make writes its own
"Entering directory" lines to stdout.

Each build also writes a `.manifest` beside the `.bin` recording the OpenBoot
revision, whether that checkout was dirty, the chip/transport/board, and the
image sha256. Hash that file into your own build identity — the image sha256
already accounts for every knob, source file and compiler choice, so it is the
whole statement about what was built.

To assert the checkout really is the pinned one before building:

```sh
python3 check_dependencies.py --expect-revision <sha> --toolchain "$MRS_TOOLCHAIN"
```

That fails if HEAD differs or the worktree is dirty. Without it, nothing
compares a vendored OpenBoot against its gitlink.

## Board configuration

Board settings live in `boards/*.mk` and may be overridden on the Make command
line.

| Setting | Default | Meaning |
|---|---|---|
| `OB_BOOT_PIN_MASK` | unset | Active-low OpenBoot entry strap; unset disables it |
| `OB_BOOT_PIN_PORT_B` | `0` | `1` selects port B; invalid on CH57x |
| `OB_IDLE_TIMEOUT_MS` | `10000` | Nominal idle auto-boot setting; `0` disables it |
| `OB_BOOT_IMAGE_CRC` | off | Check the complete image CRC on every boot |
| `OB_UART1_REMAP` | off | CH59x UART1 on PB12/PB13 instead of PA8/PA9 |
| `OB_HSE_CAP_LOAD` | `6` | CH57x-only HSE load field (`0..7` selects 6..20 pF in 2 pF steps) |

No shipped board defines an OpenBoot strap; mask-ROM ISP is the recovery path.

`OB_IDLE_TIMEOUT_MS` is a nominal setting, not a duration: it converts to a
count of main-loop iterations, and an iteration is only nominally 20 µs. Real
timeouts measured on silicon for the default `10000`:

| Image | Real timeout | Drift |
|---|---|---|
| CH570 USB at 100 MHz | ~273 s | ~27x |
| CH592 USB at 60 MHz | ~86 s | ~8.6x |

The drift is per chip and transport, so a board that cares about the real
figure has to measure it. Treat the setting as a lower bound in any case:
frame handling stretches an iteration, and on USB a send against a host that
is not draining can block for milliseconds while still crediting one slot.

## Flash the bootloader

Use either SWD through a WCH-LinkE or the chip's mask-ROM ISP:

```sh
make CHIP=ch592 TRANSPORT=uart flash       # minichlink; includes readback
make CHIP=ch592 TRANSPORT=uart flash-isp   # wchisp; chip must be in ROM ISP
```

`WCHLINK_SERIAL=<serial>` selects a probe when several are attached. The
application is normally flashed through OpenBoot itself:

```sh
openboot flash app.bin --force
```

### CH570/CH572 USB recovery

If a CH570/CH572 USB image is running, SWD cannot attach after the firmware
enables USB. Recover through mask-ROM ISP:

1. Disconnect power.
2. Pull PA1 high while reconnecting power. PA1 is the ROM-ISP entry pin and
   USB D+ on these chips. On the EVT board this is the DOWNLOAD button.
3. Confirm the ROM responds, then flash a UART build or erase the chip:

   ```sh
   wchisp info
   wchisp flash build/ch570-uart/openboot-ch570-uart.bin
   ```

PA1 is not 5 V tolerant. Pull it to the board's 3.3 V rail, or limit clamp
current appropriately; do not connect it directly to USB VBUS.

`OB_BOOT_PIN_MASK` is unrelated to the ROM-ISP pin. The shipped boards leave
that OpenBoot-specific strap disabled.

A WCH-LinkE can sometimes catch the core before USB disables debug:

```sh
minichlink -kt -l <probe-serial>   # remove probe power
minichlink -A  -l <probe-serial>   # power and halt without reboot
minichlink -E  -l <probe-serial>   # erase while halted
```

Repeat the halt immediately before each operation while the USB image remains
installed. Rehearse at least one recovery route before first bring-up.

## Tests

```sh
make test
```

The host suite compiles the production core against simulated CH57x and CH59x
flash. It covers protocol vectors, bounds and alignment, flash rules, CH57x
stale-XIP behavior, retries, and power cuts. No hardware is required.

## Hardware bring-up

Bring up UART before USB, and leave CH57x USB until last.

1. Build the image and confirm the size check passes.
2. Flash through SWD, read it back, and power-cycle.
3. Probe over UART or confirm USB enumerates as `1209:0001`.
4. Check HELLO reports the correct family, app bounds, geometry, features, and
   UID.
5. Flash, COMMIT, verify, and boot a test application.
6. Interrupt erase and write operations; each reset must return to OpenBoot and
   allow a clean reflash.
7. Test idle auto-boot and application-requested bootloader entry.
8. Exercise CRC mismatch, write-before-erase, and ROM-ISP recovery paths.

A variant remains build-validated only until this sequence passes on its own
silicon.
