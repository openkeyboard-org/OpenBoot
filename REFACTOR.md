# OpenBoot refactoring review

The current design is already fairly disciplined; most complexity is justified
by A/B safety and the CH570 stale-XIP behaviour. Existing artifacts are roughly
4.9–6.0 KiB, so source simplicity should take priority over aggressive byte
shaving.

## v0.11 implementation status

Branch `refactor/v0.11` implements the wire-compatible, product-neutral items
2, 3, 5, 6, 7, 9, 10, 11, and 13 below. OBP remains v0.2; the reported
bootloader version and host package advance to v0.11. The scope-dependent items
remain proposals.

## Highest-value refactors

1. **Make boards the primary build targets.**

   The build currently exposes four chips × two transports, then layers a board
   on top (`firmware/Makefile:52`). The actual products are only:

   - OpenDongle CH570 USB
   - OpenDongle CH592 USB
   - OpenController CH592 UART

   Let each board select its chip, transport, clock, port and geometry. Keep
   generic chip/transport builds only as optional development targets. If CH572
   and CH591 are not product requirements, removing them would also eliminate
   several mappings, matrix builds, runtime wrong-variant handling, and host
   warnings.

2. **Consolidate the duplicated family port implementation.**

   Large portions of `firmware/ports/ch57x/port_ch57x.c` and
   `firmware/ports/ch59x/port_ch59x.c` are effectively identical: flash
   wrappers, SysTick bookkeeping, delay, jump and reset.

   A small shared `port_ch5xx.c` could contain those direct functions. Family
   files would retain only clock setup, UID quirks, PHY and pin configuration.
   This preserves direct linking and avoids creating a generic HAL or vtable.

3. **Use one startup assembly file.**

   The two startup files are almost identical; only the family CSR values
   differ around `startup_openboot.S:60`. Use one file with two generated
   constants and one optional CSR write. This is a particularly clean
   duplication removal with no runtime cost.

4. **Remove the OpenBoot strap feature if the product scope is authoritative.**

   Every shipped board deliberately disables it, while mask-ROM ISP is the
   documented recovery path. Nevertheless, the port hook, debounce logic,
   board knobs, comments and artifact-policy checker remain
   (`firmware/core/boot_decision.c:195` and
   `firmware/core/openboot_port.h:127`).

   Removing it would simplify both families and the build system. Keep it only
   if external users genuinely configure custom boards with an
   OpenBoot-specific strap.

5. **Perform boot-record selection in one pass and retain the generation.**

   `ob_boot_select()` loads a record, then `ob_boot_app_valid()` loads the same
   record again (`firmware/core/boot_decision.c:124`). COMMIT later calls
   `ob_next_generation()`, reading both records again
   (`firmware/core/boot_core.c:560`).

   Have one selector return:

   - selected bootable slot;
   - selected record;
   - highest valid record generation.

   `ob_core_init()` can cache the generation and increment it after each
   successful COMMIT. This removes duplicate XIP reads and some of the
   stale-XIP generation bookkeeping.

6. **Derive the erase bitmap from slot geometry.**

   `OB_BITMAP_BYTES` is hard-coded to 16 based on the largest supported app
   region (`firmware/core/openboot_port.h:98`), while mutations can only
   address one slot.

   Size the bitmap as:

   ```c
   (slot_block_count + 7) / 8
   ```

   Index it relative to `ob_slot_base(write_slot)` rather than the whole
   application region (`firmware/core/boot_core.c:265`). This removes an
   arbitrary CH592-derived limit and saves some RAM.

7. **Simplify fixed two-slot arithmetic.**

   With exactly two contiguous equal slots:

   ```c
   slot_base = OB_SLOT_A_BASE + slot * OB_SLOT_SIZE;
   other = slot ^ 1u;
   ```

   This can replace the separate ternary helpers in `boot_decision.c` and
   `boot_core.c`. The current base helper also silently maps an invalid slot to
   slot A, which is less explicit than arithmetic after validation.

## Useful secondary simplifications

8. **Simplify product timeout measurement.**

   All product boards use a ten-second timeout, below the low SysTick word's
   fastest wrap time of roughly 43 seconds. A raw `ob_ticks()` and unsigned
   tick comparison could replace the millisecond accumulator, three port
   globals, and `ob_ms_accumulate()`.

   This is worthwhile only if long configurable timeouts are not needed.
   Otherwise, retain the current general implementation and merely move it
   into the shared port source.

9. **Normalize build booleans.**

   The generated configuration conditionally omits macros because source files
   use `#ifdef` (`firmware/Makefile:368`). Always generate numeric `0` or `1`
   values and use `#if OB_*`. That would simplify the Make conditionals and
   avoid the distinction between undefined and defined-as-zero.

10. **Derive the linker boot-request address directly from RAM geometry.**

    The linker already places the stack at `ORIGIN(RAM) + LENGTH(RAM) - 16`,
    but the Makefile parses a C header with `sed` to reproduce that address
    (`firmware/Makefile:540`). Generate the C and linker values from one numeric
    RAM-size setting, or let the linker define the address directly. This
    removes a fragile build-time parser.

11. **Collapse the Rust `Transport` and `FrameLink` implementation boilerplate.**

    The host has a public exchange trait and a second byte-level trait, followed
    by identical `xfer()` implementations in HID, UART and test transports
    (`tools/src/transport/mod.rs:23`). A default method or blanket
    implementation over `FrameLink` would preserve the layering while removing
    those repeated adapters.

12. **Specialize bundles for the actual two-slot model.**

    The firmware permanently supports two slots, but the bundle format supports
    up to eight arbitrary variants (`tools/src/bundle.rs:48`). If OpenBoot
    bundles are strictly A/B product artifacts, represent exactly the slot-A
    and slot-B images. That would remove sorting, arbitrary overlap handling
    and several ambiguous selection paths.

13. **Remove unused ISP dependencies from the application companion.**

    `firmware/app/openboot_app.c` includes both ISP headers, but the
    implementation only uses SFR definitions for safe access and reset.
    Dropping those ISP includes would make application integration smaller and
    clearer.

14. **Consider trimming informational HELLO fields before protocol 1.0.**

    `write_page` is only displayed by the host and does not affect flashing.
    Other candidates include fixed facts such as `slot_count == 2`. Remove only
    fields that have no safety or portability role; retaining geometry reported
    by the device is preferable to introducing a host-side chip database.

## Refactors to avoid

- Do not introduce a runtime port vtable or general HAL. Link-time direct
  symbols are simpler and smaller.
- Do not move the CH570 sequential CRC workaround into chip-specific command
  handlers. Keeping it capability-driven in the shared core is the better
  boundary.
- Do not conditionally compile most stream state out of CH592 unless image
  space becomes tight. It would save bytes, but produce more divergent
  firmware paths.
- Do not force the application companion and bootloader to share one CRC
  implementation. Their different deployment boundaries and size/speed choices
  justify that small duplication.
- Keep USB serial-number support; the host uses it to select among multiple
  identical dongles. Removing only manufacturer/product strings is possible,
  but offers little compared with the loss of a visible bootloader identity.

The original review was performed read-only. The subsequent v0.11 implementation
was validated with the host suites and the complete eight-image firmware matrix.
