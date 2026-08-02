# OpenDongle product dongle, CH592F, USB transport.
#
# No strap: see generic-ch59x.mk (ROM ISP is the recovery path).

# Idle auto-boot: with a valid boot record and no HELLO received, convert this
# nominal millisecond setting to a poll count. 0 disables the timeout.
# NOTE: this counts poll iterations, not wall-clock time. A dongle must never
# sit dead in the bootloader, so the timeout stays enabled; the value is
# nominal until bench-calibrated on ch592-usb (see the ch570-usb measurement
# in generic-ch57x.mk for how far a nominal setting can drift).
OB_IDLE_TIMEOUT_MS := 10000

# Full image-CRC check at every boot. The dongle's release discipline pins
# artifact digests end to end; a stale-but-still-valid boot record left in
# DataFlash after an SWD reflash (minichlink -E does not touch DataFlash)
# must refuse to boot mismatched bytes rather than silently launch them.
# Cost is milliseconds: on a USB build the CRC runs after clock init.
OB_BOOT_IMAGE_CRC := 1
