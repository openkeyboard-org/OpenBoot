# OpenDongle product dongle, CH570D, USB transport.
#
# No strap: see generic-ch57x.mk (ROM ISP via the PA1/D+ pull-up cable is
# the recovery path; a USB image clears RB_PIN_DEBUG_EN, so SWD is gone
# while the bootloader or app is running).

# Idle auto-boot, in real milliseconds. Kept enabled so a dongle never sits
# dead in the bootloader. Previously a poll count that measured ~273 s here
# for the same 10000; the value is unchanged and its meaning is what moved.
OB_IDLE_TIMEOUT_MS := 10000

# Full image-CRC check at every boot: same rationale as opendongle-ch592.mk.
OB_BOOT_IMAGE_CRC := 1

# The dongle keeps its RF bond in code flash at 0x3A000, directly below the
# OpenBoot boot record page at 0x3B000. Clamp the build's app region so no
# OBP ERASE/WRITE/COMMIT can ever reach either page; the port's
# ob_app_end() takes min(silicon, build), so this bound also holds on
# larger silicon. Covered by test_core_native.py (build-side clamp) and
# test_board_config.py (this file really lands in the generated config).
OB_APP_END := 0x0003A000
