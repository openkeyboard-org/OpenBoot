# OpenBoot application companion

`openboot_app.c` lets an application read its boot record and request an
OpenBoot session.

## Integrate the API

Compile `openboot_app.c` into the application:

```c
#include "openboot_app.h"

ob_boot_record_t record;
if (openboot_get_record(&record) == 0) {
    /* record.img_len and record.img_crc32 describe the committed image */
}

/* Write the RAM request and reset into OpenBoot; never returns. */
openboot_request_update();
```

Define exactly one chip family, plus the slot this image is linked for:

- `OPENBOOT_CHIP_CH57X` for CH570/CH572, or `OPENBOOT_CHIP_CH59X` for CH591/CH592
- `OPENBOOT_SLOT_BASE` — the base address this image links at
- `OPENBOOT_SLOT_SIZE` — the slot size OpenBoot was built with

Add `firmware/app` and the matching WCH `StdPeriphDriver/inc` directory to the
include path. Both families read the record straight out of memory-mapped code
flash, so no ROM API is involved and CH59x applications no longer need
`libISP592.a` linked for this.

The application is built **once per slot** — these parts have no flash remap,
so one image cannot execute from two bases. See
[the A/B update design](../../docs/AB-UPDATE.md).

`openboot_get_record()` checks that it can read a record with the expected
magic. Applications that depend on the record contents should also validate
`rec_crc32` over its first 28 bytes.

## Linker layout

### Flash

OpenBoot owns `[0x0000, 0x2000)`. The rest is split into two equally sized
slots, and the application is linked at a **slot base** — not at `0x2000`
unconditionally, which is only slot A:

| Chip | App region | Slot A | Slot B | Each slot |
|---|---|---|---|---|
| CH570 / CH572 | `[0x2000, 0x3C000)` | `0x2000` | `0x1F000` | 116 KiB |
| CH591 | `[0x2000, 0x30000)` | `0x2000` | `0x19000` | 92 KiB |
| CH592 | `[0x2000, 0x70000)` | `0x2000` | `0x39000` | 220 KiB |

Slot bases shift when a board sets `OB_APP_END`, so take them from the build
rather than from this table — `OPENBOOT_SLOT_BASE` and `OPENBOOT_SLOT_SIZE`
above are the values OpenBoot was built with. For example, slot A on CH592:

```ld
FLASH (rx) : ORIGIN = 0x00002000, LENGTH = 216K
```

Each slot reserves its final 4096-byte block for its own boot record, so an
application may use `slot_size - 4096` bytes. The device reports the base and
capacity it will accept over HELLO (`write_base`, `write_capacity`), and that
base alternates from one update to the next.

Nothing on the device can tell one slot's build from the other's. OBP carries
addresses, a length and a CRC — never a link base — so a device asked to write
slot-A bytes at slot B's base will do exactly that, and commit an image that
cannot run there. **Choosing the artifact that matches `write_slot` is the
host's job.** The `openboot` CLI refuses a mismatch before anything is erased;
any other host must make the same check itself. See
[the A/B update design](../../docs/AB-UPDATE.md).

### RAM boot request

Reserve the top 16 bytes of SRAM so the request magic survives a software
reset. Limit all sections and the stack to the range below the reservation:

| Family | Usable RAM region | Stack top |
|---|---|---|
| CH570 / CH572 | `[0x20000000, 0x20002FF0)` | `0x20002FF0` |
| CH591 / CH592 | `[0x20000000, 0x200067F0)` | `0x200067F0` |

For CH57x, for example:

```ld
RAM (xrw) : ORIGIN = 0x20000000, LENGTH = 0x2FF0
_eusrstack = ORIGIN(RAM) + LENGTH(RAM);
```

The addresses are defined as `OB_BOOTREQ_ADDR_CH57X` and
`OB_BOOTREQ_ADDR_CH59X` in `protocol/openboot_protocol.h`. OpenBoot clears the
request after consuming it.

## Factory install

A blank part needs the bootloader and the application written together. Build
a whole-chip image with the bootloader, a pad, and the application:

```sh
make -C firmware CHIP=ch592 TRANSPORT=usb factory APP=/path/to/app.bin
```

The result is `<image>-factory.bin` in the build directory, written at flash
address `0`. The application lands at the same file offset as its load
address, so nothing has to be positioned by hand.

> [!IMPORTANT]
> The pad between the bootloader and `0x2000` is `0x00`, never `0xFF`.
> Programming `0xFF` on CH5xx programs nothing, so an `0xFF` pad would never
> reach flash and a post-flash readback compare would fail across the whole
> pad region. `firmware/compose_factory.py` owns this rule; use it rather than
> concatenating by hand.

An application written this way has no boot record, so the device comes up in
the bootloader. Attest it once over OBP and boot:

```sh
openboot bless app.bin
openboot boot
```

### CH59x DataFlash

OpenBoot no longer reserves any DataFlash: records live inside their slots, in
code flash, on both families. All 32 KiB of CH59x DataFlash is available to the
application. CH57x has no DataFlash at all.
