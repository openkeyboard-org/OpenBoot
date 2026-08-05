# OpenBoot firmware

The firmware builds one bootloader image for each supported chip and
transport. Each image occupies flash `[0x0000, 0x2000)`; the application starts
at `0x2000`. The region above it is split into two A/B slots so an
interrupted update leaves the previous application bootable.

> [!WARNING]
> CH570/CH572 USB builds disable SWD because USB shares PA0/PA1 with the debug
> interface. Read [CH570/CH572 USB recovery](#ch570ch572-usb-recovery) before
> flashing one. UART builds leave SWD enabled and are the safer bring-up path.

See the [protocol specification](../docs/PROTOCOL.md),
[architecture](../docs/ARCHITECTURE.md), the
[A/B update design](../docs/AB-UPDATE.md), and the
[application integration guide](app/README.md) for related details.

## Prerequisites

| Tool | Requirement |
|---|---|
| GNU Make | 4.3 or newer |
| MounRiver GCC | "RISC-V Embedded GCC" 12 (15 builds but is unvalidated) |
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

**Use GCC 12.** It is the only compiler validated on silicon.

GCC 15 also builds the whole matrix, and the build detects its renamed tools
(`riscv-wch-elf-*` became `riscv32-wch-elf-*`), so it is usable for
development. It is not validated: on a CH572 it produced an idle auto-boot
that fired early and erratically — 1.51 s, 1.71 s and 4.75 s against a
configured 10 s — where GCC 12 on the same part gave 9.96 s every time, and it
failed to return from a software reset 4 times in 32 against 0 in 32 for
GCC 12. ch59x showed no such behaviour. The generated code for the timing
functions is instruction-identical between the two, so the cause is elsewhere
and unresolved. `check-deps` warns on it and builds anyway.

The two also emit slightly different code (−12 to +80 bytes per image), so an
image is only byte-reproducible against the compiler that built it.

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
| `OB_IDLE_TIMEOUT_MS` | `10000` | Idle auto-boot deadline in milliseconds; `0` disables it |
| `OB_BOOT_IMAGE_CRC` | off | Check the complete image CRC on every boot |
| `OB_UART1_REMAP` | off | CH59x UART1 on PB12/PB13 instead of PA8/PA9 |
| `OB_HSE_CAP_LOAD` | `6` | CH57x-only HSE load field (`0..7` selects 6..20 pF in 2 pF steps) |
| `OB_APP_END` | silicon end | Shrink the app region so OBP cannot reach flash the board reserves |
| `OB_USB_VID` / `OB_USB_PID` | `0x1209` / `0x0001` | USB identity; set both or neither |

No shipped board defines an OpenBoot strap; mask-ROM ISP is the recovery path.

`OB_APP_END` fences OBP off from flash the board reserves for something else —
OpenDongle's CH570 keeps its RF bond at `0x3A000`, just below the boot-record
page, so its board file sets `OB_APP_END := 0x0003A000` and no `ERASE`,
`WRITE` or `COMMIT` can reach either page. It must be 4096-aligned and inside
`(app base, silicon end]` — above `0x2000` and no higher than the silicon's own
end; the device advertises the resulting bound over HELLO, so the host needs no
per-board knowledge.

`OB_IDLE_TIMEOUT_MS` is wall-clock milliseconds, measured against a
free-running SysTick counter that the port starts once the clock is settled.
It used to be a poll count merely named after milliseconds, which drifted
badly under load — the default `10000` measured about 273 seconds on CH570 USB
and about 86 seconds on CH592 USB. The value is unchanged; what moved is that
it now means what it says.

`OB_USB_VID` / `OB_USB_PID` let a product enumerate its bootloader under its
own USB identity instead of a bootloader-specific one, so a user sees one
device changing mode rather than two. Set both or neither. Doing this makes
VID:PID ambiguous — the application's own HID interfaces sit behind the same
pair — so the host distinguishes the bootloader by its vendor usage page
`0xFF00` usage `0x01`, and the application must not use that pair. The
`openboot` CLI already filters this way.

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
3. Probe over UART, or confirm USB enumerates as the board's configured
   identity — `1209:0001` unless `OB_USB_VID`/`OB_USB_PID` are set, which the
   product boards do (`0C45:FEFE`).
4. Check HELLO reports the correct family, app bounds, geometry, features,
   UID, and slot layout — on a blank part, no active slot and slot A as the
   write target.
5. Flash, COMMIT, verify, and boot a test application.
6. Flash a second time and confirm the update lands in the OTHER slot, that
   the device boots it, and that both images are present in flash at once.
7. Interrupt erase and write operations. With a previous application
   installed, each reset must come back up running that application unaided;
   on a part with nothing installed yet, it must return to OpenBoot and allow
   a clean reflash.
8. Test idle auto-boot and application-requested bootloader entry.
9. Exercise CRC mismatch, write-before-erase, and ROM-ISP recovery paths.

A variant remains build-validated only until this sequence passes on its own
silicon.

Steps 6 and 7 are automated in [`tests/bench/`](tests/bench/), which drives a
part through the slot lifecycle and cuts power at three points of an update.
It is bench-specific (probe serials and ports) and is not part of `make test`.
CH572 and CH592 pass it; CH570 and CH591 remain build-validated.

One trap it documents, because it wastes an afternoon otherwise: **opening or
closing the WCH-Link CDC port resets the target**, so any UART-based check of
"did it boot the application?" perturbs the answer. Observe boot outcomes over
SWD.
