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

Define exactly one chip family:

- `OPENBOOT_CHIP_CH57X` for CH570/CH572
- `OPENBOOT_CHIP_CH59X` for CH591/CH592

Add `firmware/app` and the matching WCH `StdPeriphDriver/inc` directory to the
include path. CH59x applications must also link `libISP592.a`; CH57x reads the
record directly from memory-mapped code flash.

`openboot_get_record()` checks that it can read a record with the expected
magic. Applications that depend on the record contents should also validate
`rec_crc32` over its first 12 bytes.

## Linker layout

### Flash

OpenBoot owns `[0x0000, 0x2000)`. Link the application at `0x2000` and keep it
within the family-specific region:

| Chip | App region | Length |
|---|---|---|
| CH570 / CH572 | `0x2000..0x3B000` | 228 KiB |
| CH591 | `0x2000..0x30000` | 184 KiB |
| CH592 | `0x2000..0x70000` | 440 KiB |

For example:

```ld
FLASH (rx) : ORIGIN = 0x00002000, LENGTH = 228K
```

Do not reuse the vendor CH570 EVT application's 235 KiB length: it overlaps
OpenBoot's boot-record block at `0x3B000`.

### RAM boot request

Reserve the top 16 bytes of SRAM so the request magic survives a software
reset. Limit all sections and the stack to the range below the reservation:

| Family | Usable RAM region | Stack top |
|---|---|---|
| CH570 / CH572 | `0x20000000..0x20002FF0` | `0x20002FF0` |
| CH591 / CH592 | `0x20000000..0x200067F0` | `0x200067F0` |

For CH57x, for example:

```ld
RAM (xrw) : ORIGIN = 0x20000000, LENGTH = 0x2FF0
_eusrstack = ORIGIN(RAM) + LENGTH(RAM);
```

The addresses are defined as `OB_BOOTREQ_ADDR_CH57X` and
`OB_BOOTREQ_ADDR_CH59X` in `protocol/openboot_protocol.h`. OpenBoot clears the
request after consuming it.

### CH59x DataFlash

OpenBoot reserves DataFlash offsets `0x7000..0x7FFF` for the boot record.
Applications may use `0x0000..0x6FFF` but must not erase or write the reserved
block. CH57x has no DataFlash; its record block at `0x3B000` is already outside
the application region.
