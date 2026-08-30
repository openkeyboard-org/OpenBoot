# OpenBoot's flash driver vs ch32fun

OpenBoot links no vendor blob; `ports/flash_ch5xx.c` is our own CH5xx flash
driver, written from a full disassembly of WCH's libISP archives
(`libISP572.a` / `libISP592.a`). ch32fun's `extralibs/ch5xx_flash.h` (MIT) is
an independent reverse-engineering of the same controller and was a valuable
**cross-check** while writing ours — but it is not something we can adopt
as-is. This note records the specific divergences so the choice is
discoverable, and so the fixes can be offered upstream later.

Line references are to ch32fun `extralibs/ch5xx_flash.h` as read 2026-08-30.
"the blob" means the disassembled vendor archive, which is the behavioral
reference our driver reproduces per family.

## Correctness defects (vs the blob)

- **Inverted erase-alignment test** (`:235-244`). On ch59x the selector is
  ```c
  else if (len >= 4096 && (addr & (4096 - 1))) { cmd = 0x20; /* sector */ }
  else                                          { cmd = 0x81; /* page   */ }
  ```
  `(addr & 4095)` is true only when the address is **misaligned**, so a normal
  4 KiB-*aligned* erase falls through to `0x81` and runs as 16× page erase.
  Page erase is unsupported on **CH592A** (datasheet §4.4) and hangs it beyond
  SWD recovery — so ch32fun bricks a CH592A on its first aligned erase, and on
  parts that do support it (e.g. CH592F) it still does 16× the ops and wear for
  no reason. (OpenBoot verified `0x81` works on CH592F, but keeps it behind an
  explicit opt-in — `OB_FLASH_PAGE_ERASE`, default off — for exactly this
  reason; see the `OB_FL_ERASE_OP` hazard note in `flash_ch5xx.c`.)

- **Under-clocked verify** (`:259-278`). The loop is bounded by `len32` (the
  word count) while `ch5xx_flash_rom_in()` clocks one byte per iteration, so it
  reads only `len/4` bytes for a `len`-byte buffer, and it compares
  `R32_FLASH_DATA` at `i == 0` — before four bytes have been clocked into the
  word register. It cannot correctly verify the buffer. Ours clocks all `len`
  bytes and compares each assembled word (bench-proven by the soak / CRC
  parity checks).

- **Wrong gate bit on close** (`:114`). `R8_GLOB_ROM_CFG &= RB_ROM_CTRL_EN`
  preserves `CTRL_EN` (0x20) and clears `CODE_OFS` (0x10) — the opposite of the
  blob, which does `reg & RB_ROM_CODE_OFS` (preserve the code-offset selector,
  drop the control-enable). This leaves the ROM control interface enabled and
  can flip the code-offset mapping after every operation.

## Blob-inexact sequencing

- **Gate value** (`:106`): erase/write opens with `RB_ROM_CTRL_EN |
  RB_ROM_CODE_WE` = `0xA0`, omitting `RB_ROM_DATA_WE`; the blob (and our port)
  use `0xE0`.
- **Not per-family** (`:66-77`): `ch5xx_flash_rom_begin` always emits 2 NOPs
  and double-sends the `0xFF` resume. That is the ch57x/ISP572 shape; the
  ch59x/ISP583 blob uses 1 NOP and a single send. ch32fun applies the ch57x
  form to both families. Our driver keeps these as per-family constants
  (`OB_FL_BEGIN_NOPS`, `OB_FL_RESUME_TWICE`).

## Integration mismatches for a polled bootloader

- **`close()` re-enables interrupts** (`:117`, `__enable_irq()`): OpenBoot runs
  with `MIE = 0` for the bootloader's entire life (fully polled, no ISRs).
  Adopting this close would turn interrupts on mid-boot.
- **No `0x4B` unique-ID read**: ch32fun exposes only the `0x0B` info-window
  read; `ob_family_read_uid()` needs the `0x4B` UID (XOR-folded), plus the
  CH570 MAC fallback.
- **XIP read path** (`:177`, `ch5xx_flash_cmd_read` does `*(uint32_t*)addr`):
  our verify is deliberately a controller fast-read, never XIP, because of the
  ch57x F26 stale-XIP erratum.
- **Fixed timeout** (`:139`, `524288` iterations): the blob's clock-independent
  spin, which we replaced with an `OB_CPU_HZ`-derived bound — UART images run
  the CPU at 6.4 MHz, ~16× slower than the USB clocks, so a fixed count gives
  wildly different wall-times.
- **Return contract**: `0` / `(uint32_t)-1` vs our namespaced, nonzero-low-byte
  `OB_FLERR_*` codes that travel as the `E_FLASH` wire detail (detail 0 is
  reserved; see `flash_ch5xx.h`).

## Why our own port

Ours is written from the actual blob disassembly (behavior-exact **per
family**, where ISP572 and ISP583 genuinely differ), fixes all of the above,
and is validated: byte-identical parity against the vendor archive before it
was dropped, 57 host unit tests over a recording register mock,
`check_highcode.py` RAM-self-containment enforcement, integration with the
protocol's `E_FLASH` wire contract, and a full CH592/CH570 bench campaign
(soak, real power cuts, A/B acceptance, both families, both clock regimes).

## Upstreaming

These are candidate fixes to offer ch32fun. The inverted-test erase in
particular is worth an upstream bug report: any CH59x user on a new die hits
the page-erase hang on their first aligned erase. (Tracked here as a to-do,
not a commitment.)
