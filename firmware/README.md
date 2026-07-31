# OpenBoot firmware

The firmware builds one bootloader image for each supported chip and
transport. Each image occupies flash `0x0000..0x1FFF`; the application starts
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
| MounRiver GCC | GCC 12 `riscv-wch-elf-*` toolchain |
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
`make check-deps` verifies the pinned SDK revisions and expected compiler.

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

## Board configuration

Board settings live in `boards/*.mk` and may be overridden on the Make command
line.

| Setting | Default | Meaning |
|---|---|---|
| `OB_BOOT_PIN_MASK` | unset | Active-low OpenBoot entry strap; unset disables it |
| `OB_BOOT_PIN_PORT_B` | unset | Use port B; invalid on CH57x |
| `OB_IDLE_TIMEOUT_MS` | `10000` | Approximate idle auto-boot interval; `0` disables it |
| `OB_BOOT_IMAGE_CRC` | off | Check the complete image CRC on every boot |
| `OB_UART1_REMAP` | off | CH59x UART1 on PB12/PB13 instead of PA8/PA9 |
| `OB_HSE_CAP_LOAD` | `6` | CH57x-only HSE load field (`0..7` selects 6..20 pF in 2 pF steps) |

No shipped board defines an OpenBoot strap; mask-ROM ISP is the recovery path.
The idle timeout counts poll iterations, so it is a lower bound rather than a
wall-clock deadline under load.

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
