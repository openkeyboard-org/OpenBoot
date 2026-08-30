# A/B update design

How OpenBoot survives an interrupted update. This note records the invariant
and the reasoning; the wire format lives in [PROTOCOL.md](PROTOCOL.md).

## The problem

Today OpenBoot erases the application in place, and `ob_record_invalidate()`
clears the boot record at the **first mutation of every session**. So for the
entire duration of every update — seconds — the device has no valid record. A
power cut, a yanked cable or a crashed host anywhere in that window leaves it
in the bootloader, needing a host to recover. That is the documented behaviour,
and it is what this design replaces.

The goal: an interrupted update leaves the **previous image running**, with no
host involvement.

## Constraints

Verified against the vendor SDK headers and the datasheets vendored in
`third_party/`, not assumed:

| | CH570 / CH572 | CH591 / CH592 |
|---|---|---|
| Minimum erase | 4096 B (code flash) | 4096 B (code flash) |
| Minimum write | 4 B | 4 B (code flash) |
| DataFlash | none | 32 KiB, 256 B page erase **documented but unusable** |
| Record lives in | code flash, inside its slot | code flash, inside its slot |

Records moved into the slots, so the CH59x DataFlash path is gone entirely and
both families read a record as a plain memory-mapped word. The blocks the old
shared record used have been **reclaimed**: ch57x's app region grows to
`[0x2000, 0x3C000)`, all of CodeFlash and the `FLASH_ROM_MAX_SIZE` limit in
`ISP572.h`, and all 32 KiB of CH59x DataFlash is now free for applications.

Three facts do most of the work here:

- **Page erase is opt-in, ch59x only.** ch57x (ISP572) exposes no page-erase
  command at all; 4096 is the floor there. On ch59x, 256 B page erase (`0x81`)
  is real: the CH592 datasheet §4.4 says *"CH592A does not support page erase"*
  and a CH592A hangs beyond SWD recovery, but a later register-level probe
  proved it works and is selective on **CH592F** (`firmware/tests/silicon/`) —
  the earlier bench hang was an implementation issue, not the die. Because
  `CHIP=ch592` cannot tell the A and F dies apart, page erase ships behind the
  `OB_FLASH_PAGE_ERASE` build knob (default off): setting it lowers the erase
  granularity to 256 B and is the builder's assertion that their die supports
  it. The default build treats 4096 as the minimum erase on both families.
- **No flash remap.** The only address-related bit, `RB_ROM_CODE_OFS`, is a
  fixed boot-entry selector, not a translation.
- **The application cannot be relocated.** The vendor blobs are
  `-mcmodel=medany`: `LIBCH59xBLE.a` alone holds 994 PC-relative references to
  COMMON symbols and 172 absolute `R_RISCV_32` pointer entries, none of which
  survive a base shift, and they cannot be recompiled.

The last one is why slot B holds a **second build** of the application rather
than a copy of slot A's bytes, and therefore why the slots are equal.

## Layout

The application region splits into two equal slots, each 4096-aligned, any
remainder left unused above slot B.

| Build | Slot A | Slot B | Each |
|---|---|---|---|
| ch570 / ch572 (`0x3C000`) | `0x02000` | `0x1F000` | 116 KiB |
| ch570 clamped (`0x3A000`) | `0x02000` | `0x1E000` | 112 KiB |
| ch591 (`0x30000`) | `0x02000` | `0x19000` | 92 KiB |
| ch592 (`0x70000`) | `0x02000` | `0x39000` | 220 KiB |

Product images are 25,056 B (CH570) and 45,644 B (CH592), so both slots have
roughly 4x headroom.

## The record

Each slot is **self-describing**: a 32-byte record in the **final erase block**
of its own slot, at `slot_base + slot_size - 4096`, written at COMMIT.

The record owns a whole 4096-byte block rather than just its 32 bytes because
rewriting it means erasing it first — flash only clears bits — and erase
granularity is one block. If an image could reach into that block, re-committing
a slot would destroy image bytes. The block costs 4 KiB per slot (1.8% on ch592,
3.6% on ch570) and makes every record rewrite safe. Usable image size is
therefore `slot_size - 4096`.

| Offset | Size | Field |
|---|---|---|
| 0 | 4 | `magic` — `"OBR2"` |
| 4 | 4 | `generation` — monotonic; the higher valid record wins |
| 8 | 4 | `img_len` — bytes from the slot base |
| 12 | 4 | `img_crc32` — over `[slot_base, slot_base + img_len)` |
| 16 | 12 | reserved, zero |
| 28 | 4 | `rec_crc32` — over bytes `[0, 28)` |

There is **no shared metadata block**. Nothing outside the slot being updated
is written or erased at any point in an update. That is the whole design.

## The invariant

> At every instant, at least one slot holds a CRC-valid record describing an
> image that passes the boot check, and the bootloader boots the highest valid
> generation.

"Passes the boot check" is weaker than "CRC-valid" unless the board asks for
more. By default the bootloader validates the record and that the slot's first
word is not the erased pattern; with `OB_BOOT_IMAGE_CRC=1` it also recomputes
the whole image CRC. Shipped product boards set that knob, and any board relying
on A/B to survive out-of-band corruption should — the record alone proves the
image was verified *at commit time*, not that it is still intact.

Update order — erase inactive slot, write image, verify, **write that slot's
record last**:

| Power cut during | Slot A record | Slot B record | Boots |
|---|---|---|---|
| erase slot B | intact, gen N | gone | **A** |
| write B image | intact, gen N | gone | **A** |
| write B record, partial | intact, gen N | CRC fails | **A** |
| write B record, complete | intact, gen N | valid, gen N+1 | **B** |

Every case is deterministic and fails closed. The record write is the commit
point, and it is the only step that changes which slot boots.

### What this does not guarantee

- **A factory-flashed part has no valid record in either slot** and lands in
  the bootloader, needing one `openboot bless`. Same as today.
- **`OBR1` records do not migrate.** Existing devices land in the bootloader
  and need one `bless`. Deliberate: OBP is pre-1.0, nothing is frozen, and a
  compatibility path would outlive the problem.
- A partial record write that *happens* to pass CRC-32 would be accepted. That
  is a ~2^-32 event and CRC-coded commit is the standard mitigation, not an
  absolute one.

### Invariants the implementation must hold

- The record sits outside the declared `img_len` and is bounds-checked before
  use.
- The record's erase block contains nothing else, so erasing it destroys no
  image bytes.
- The application linker reserves the top 4096 bytes of its slot.
- A slot is bootable only after its record validates.
- A slot the silicon is too small to contain is unusable **wholesale**, never
  shrunk — shrinking would move the record away from the address the
  application was linked against.

## Rejected alternatives

**Erase-then-rewrite a shared record** (today's scheme, extended to slots).
Leaves a window with no valid record on every update. This is what we are
fixing.

**Append-log of records in one 4096 B block.** 128 entries, appended so the
previous record survives a partial write; erase only when full. Better, but a
power cut during the erase-when-full leaves *no* valid record — the failure it
was meant to remove, merely made 128x rarer.

**Unary/thermometer slot indicator.** A dedicated block used as a write-once
counter, parity selecting the slot, with the erase-at-exhaustion safe because
both endpoints share parity. Elegant, and it removes the gap — but it has no
structural validation. An interrupted erase can leave a *stable* arbitrary
pattern rather than a transient one, and a partially programmed word can read
zero on one boot and non-zero on another across voltage and temperature,
flipping the parity. Re-programming the word to heal it proves nothing: a word
that reads zero may still have inadequate program margin, and a normal read
cannot detect that. A CRC-coded record fails validation deterministically; a
bare all-zero token cannot.

**Two-block ping-pong.** Removes the window, but needs a second erase block
adjacent to the record. On CH570 the block below the record page is
OpenDongle's RF bond store, so it would force a product data migration, and on
CH592 it would only work in DataFlash — two structurally different ports.

The generation-in-slot-record design needs none of them: with no shared mutable
state, there is no erase window to close.

*The thermometer scheme and the push away from the append-log came from design
discussion; the failure modes that ruled it out were identified in adversarial
review.*
