# OpenDongle product dongle, CH592F, USB transport.
#
# No strap: see generic-ch59x.mk (ROM ISP is the recovery path).

# Idle auto-boot: with a valid boot record and no HELLO received, convert this
# nominal millisecond setting to a poll count. 0 disables the timeout.
# NOTE: this counts poll iterations, not wall-clock time. A dongle must never
# sit dead in the bootloader, so the timeout stays enabled. Bench-measured on
# the OpenDongle CH592 board (2026-08-02): this nominal 10000 gives ~86 s of
# real time on ch592-usb (drift ~8.6x). Kept as-is for now; retuning toward
# ~10 s real means ~1160 here and moves the shipped image digest, so it rides
# with the next re-pin.
OB_IDLE_TIMEOUT_MS := 10000

# Full image-CRC check at every boot. The dongle's release discipline pins
# artifact digests end to end; a stale-but-still-valid boot record left in
# DataFlash after an SWD reflash (minichlink -E does not touch DataFlash)
# must refuse to boot mismatched bytes rather than silently launch them.
# Cost is milliseconds: on a USB build the CRC runs after clock init.
OB_BOOT_IMAGE_CRC := 1
