# OpenDongle product dongle, CH592F, USB transport.
#
# Recovery: mask-ROM ISP; see firmware/README.md.
override CHIP := ch592
override TRANSPORT := usb

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

# Enumerate under the dongle's own USB identity rather than a separate
# bootloader one, so a user sees one device that changes mode rather than two
# unrelated ones. VID:PID alone is then ambiguous: the host distinguishes the
# bootloader by its vendor HID usage page 0xFF00 usage 0x01, which the
# application's interfaces (0xFFFF and 0xFF60) deliberately do not use.
OB_USB_VID := 0x0C45
OB_USB_PID := 0xFEFE
