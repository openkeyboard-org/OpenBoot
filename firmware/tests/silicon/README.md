# CH592F page-erase (0x81) silicon probe

Investigation (2026-08-30): the OpenBoot driver only ever issues the 4 KiB
sector erase (0x20), partly because the CH592 datasheet §4.4 says *"CH592A
does not support page erase"* and an earlier bench session saw a **hang**
when a 256 B page erase (0x81) was issued — but the datasheet note names the
CH592**A** (QFN28) while our parts are CH592**F** (QFN32), and no
CH592F-specific workaround exists upstream. This probe answers: is the hang
real CH592F silicon, or was it specific to how OpenBoot drove the erase?

## Method — why a standalone firmware, not the driver

`probe_fw.c` is a minimal image that drives the flash controller directly to
issue a sector erase (control) and a page erase (test) and report exactly
where — if anywhere — either wedges. It **shares no code with
`ports/flash_ch5xx.c`**; every register write is spelled out here. It reuses
only the proven vectorless ch59x reset idiom (`probe_start.S`) so the part is
brought up correctly, and its flash routines run from RAM (`.highcode`)
exactly as any correct implementation must — opening the write gate kills
XIP, so an erase driven from flash hangs *itself*, and that self-inflicted
hang (not the die) is the most likely cause of the original bench symptom.
The built-in **control** guards the result: the sector erase (0x20 —
identical code, one opcode byte different) must complete first, so a probe
bug can't be mistaken for a silicon hang.

Phase markers (STAGE / per-op phase / heartbeat / WIP-poll iterations) land
in fixed RAM words; `minichlink` — the authoritative reader here — halts the
core afterward and reads them, plus the scratch flash. Nothing can brick:
flash keeps a bootloader and ROM-ISP recovers any state.

## Result — 0x81 works on this CH592F, repeatably

Bench CH592F (probe CEBD8F0653EF), 5 independent power-cycle trials, all
identical:

- **No hang.** STAGE reaches 0xC0 (every opcode returned); both the sector
  erase and the page erase reach phase 6 (gate closed / done) with WIP
  cleared after ~1013 poll iterations. The page erase is not slower or
  stuck versus the sector erase.
- **A true, selective page erase.** Programming all four 256 B pages of a
  sector, then 0x81-erasing only page 1, leaves (authoritative minichlink
  read of 0x6F000):
  - page 0: `A5A5A500 A5A5A501 …` — programmed, byte-exact
  - page 1: `F3F9BDA9 …`         — erased (the normal CH5xx erased word)
  - page 2: `A5A5A500 …`         — programmed, intact
  - page 3: `A5A5A500 …`         — programmed, intact
  0x81 erases exactly one page, not the whole sector, and the erased page
  reads the same `0xF3F9BDA9` pattern a 0x20 sector erase leaves.
- Corroboration: `R8_CHIP_ID` reads 0x92, and this die's `0x7F010` low byte
  is 0xFF (not 9 = `DEF_CHIP_ID_CH592A`), so WCH's own SDK guard would not
  classify it as the page-erase-hang die.

Evidence transcripts: `../../../OpenBoot-evidence/2026-08-30/50..52-*`.

## Conclusion

On **this** CH592F part, 0x81 page erase is not a silicon defect — it
completes and works correctly. The earlier bench hang was therefore
implementation-specific (most plausibly the erase being driven such that an
instruction fetch hit dead XIP mid-operation), **not** the CH592F's
page-erase command.

**What OpenBoot does with this.** Sector erase (0x20) stays the default and
universally supported mode — it is guaranteed on every die, whereas page erase
is proven only here on one CH592F, the datasheet disallows it on the CH592A
variant, and both die populations circulate. So page erase (0x81) ships as an
opt-in, default-off, ch59x-only build option (`OB_FLASH_PAGE_ERASE`) that the
builder enables only on a die verified to support it — this probe is that
verification. The finding narrows the *cause* of the old hang (ours, not the
silicon) and retires the implication that CH592F page erase is inherently
unusable.

## Note on WCH OpenOCD (`wch-ch59x.cfg`)

The cleaner method — load the probe into RAM over a debugger and run it —
was blocked: WCH OpenOCD's `wlinke` driver would not establish SDI to the
CH592 with the stock `wch-riscv.cfg`, though it connects to a CH570 with the
same file. The reason is a genuine chip-type mismatch: MRS's per-chip config
gives **CH570/2 id 139, CH59x id 11** — different WCH-Link chip-type
indices — and the stock cfg sets none. `wch-ch59x.cfg` here adds
`wlink_set_index 11`; it still did not connect on this probe/build combo
(a deeper wlinke/CH59x issue), so the firmware-probe path above was used
instead. The index finding is kept for future CH59x OpenOCD work, where it
is a necessary (if here not sufficient) piece.
