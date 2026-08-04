# OpenDongle product dongle, CH592F, USB transport.
#
# No strap: see generic-ch59x.mk (ROM ISP is the recovery path).

# Idle auto-boot: with a valid boot record and no HELLO received, boot the app
# after this many milliseconds. A dongle must never sit dead in the
# bootloader, so the timeout stays enabled.
#
# This is now 10 real seconds. It previously counted poll iterations, which
# bench-measured at ~86 s here (drift ~8.6x) for the same 10000 — the value is
# unchanged and its meaning is what moved.
OB_IDLE_TIMEOUT_MS := 10000

# Full image-CRC check at every boot. The dongle's release discipline pins
# artifact digests end to end; a stale-but-still-valid boot record left in
# DataFlash after an SWD reflash (minichlink -E does not touch DataFlash)
# must refuse to boot mismatched bytes rather than silently launch them.
# Cost is milliseconds: on a USB build the CRC runs after clock init.
OB_BOOT_IMAGE_CRC := 1
