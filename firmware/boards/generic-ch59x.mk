# Generic CH591/CH592 bring-up board.
#
# A board .mk maps hardware choices onto -D knobs (the Makefile does the
# quoting and injection). Real boards get their own file next to this one and
# are selected with BOARD=<name> on the command line.

# NO stay-in-bootloader strap, matching ch57x.
#
# Forcing a device into a bootloader with a pin is a CATASTROPHIC-recovery
# mechanism, and the silicon already provides one: the mask-ROM ISP entry pin
# (PB22 on ch59x, PA1 = USB D+ on ch57x). That path is in ROM, cannot be
# broken by anything OpenBoot flashes, and is the right tool for the job.
# OpenBoot does not layer a second strap concept on top of it.
#
# The remaining ways into OpenBoot are: the app calling
# openboot_request_update(), or an absent/invalid boot record. A device that
# boots a healthy-but-wedged app is recovered through the ROM ISP, not
# through OpenBoot.
#
# OB_BOOT_PIN_MASK intentionally unset -> ob_bootpin_asserted() compiles to
# `return 0`. `make board-policy` enforces this for both families.

# Idle auto-boot: with a valid boot record and no HELLO received, convert this
# nominal millisecond setting to a poll count. 0 disables the timeout.
# NOTE: this counts poll iterations, not wall-clock time.
OB_IDLE_TIMEOUT_MS := 10000
