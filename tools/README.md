# `openboot` host tool

`openboot` is the Rust CLI for probing and updating OpenBoot devices over USB
HID or UART. It runs on Linux, macOS, and Windows and learns flash geometry and
chip identity from HELLO.

## Build

```sh
cargo build --release
cargo test
```

The binary is `target/release/openboot`. Linux requires the `libudev`
development package (for example, `libudev-dev` on Debian/Ubuntu); macOS and
Windows need no additional system library.

## Usage

Read-only probing is the default. `flash` and `erase` show a plan unless
`--force` is supplied.

```sh
openboot                                      # probe USB HID
openboot probe --serial 0123456789ABCDEF      # select USB device
openboot --port /dev/ttyUSB0 probe            # probe UART
openboot flash app.bin                        # dry-run
openboot flash app.bin --force                # erase, write, commit, verify, boot
openboot flash app.hex --force --no-boot
openboot verify app.bin
openboot erase --all --force
openboot erase --start 0x2000 --length 0x2000 --force
openboot boot
openboot boot --stay
openboot bless app.bin
```

Global selectors:

- `--transport usb|uart` (USB by default)
- `--port PATH` (implies UART; there is no port auto-scan)
- `--serial SN` (implies USB; serial is the 16-hex-digit ROM UID)
- `--vid` and `--pid` (defaults `0x1209:0x0001`; a product may ship its
  bootloader on its own identity, in which case pass both)

Flat binaries default to base `0x2000`; use `--base` to override it. Intel HEX
files use addresses from their records. Images are padded to a 4-byte boundary
with `0xFF`. `flash` and `bless` require the image to start at the
device-reported application base.

Exit status is `0` for success, `1` for device, I/O, or usage errors, and `2`
for a verification mismatch. Idempotent commands are retried up to three times
with fresh sequence numbers. BOOT is never retried because a lost response may
mean the device already reset.

## USB permissions

USB uses a vendor-page HID interface and needs no custom driver. The tool
selects on VID:PID **plus** HID usage page `0xFF00` usage `0x01`, so a device
whose bootloader shares its application's VID:PID is still found unambiguously.

Linux users may need a udev rule for the hidraw node:

```udev
# /etc/udev/rules.d/70-openboot.rules
SUBSYSTEM=="hidraw", ATTRS{idVendor}=="1209", ATTRS{idProduct}=="0001", MODE="0660", TAG+="uaccess"
```

Reload the rules and reconnect the device:

```sh
sudo udevadm control --reload-rules
sudo udevadm trigger
```

For UART, use the platform's normal serial permissions and port name, such as
`/dev/ttyUSB0`, `/dev/cu.usbserial-*`, or `COMn`.

## UART wiring

UART is fixed at 115200 8N1, 3.3 V, with no flow control. Pins are from the
device's perspective; cross TX and RX to the adapter and connect ground.

| Family | Device TX | Device RX |
|---|---|---|
| CH570 / CH572 | PA3 | PA2 |
| CH591 / CH592 | PA9 | PA8 |

CH59x boards may remap UART1 to PB13 TX / PB12 RX with `OB_UART1_REMAP`.

## Bless an SWD-flashed image

An application written directly through SWD has no OpenBoot boot record.
`bless` computes the file CRC and sends a zero-write COMMIT:

```sh
minichlink -w app.bin 0x2000
openboot bless app.bin
openboot boot
```

On CH57x, bless must run before OpenBoot performs any flash mutation in that
power cycle. If it returns a non-sequential verification error, power-cycle and
try again. A mismatch exit status means flash does not match the padded input
file.

## CH57x verification caveat

CH570/CH572 may return stale XIP data after a flash-controller write in the
same power cycle. They clear `OB_FEAT_CRC_LIVE`, so `flash` relies on COMMIT's
stream CRC and skips the live post-COMMIT CRC check. Run `verify` after a power
cycle for an authoritative result. CH59x reports live CRC support.

The tests require no hardware. They cover protocol synchronization and golden
frames, image parsing, retry rules, command flows, and verification errors.
