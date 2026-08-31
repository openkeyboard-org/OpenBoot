# Generic CH591/CH592 bring-up board.
#
# A board .mk maps hardware choices onto -D knobs (the Makefile does the
# quoting and injection). Real boards get their own file next to this one and
# are selected with BOARD=<name> on the command line.

# No stay-in-bootloader strap: OpenBoot does not have one. Forcing a device
# into a bootloader with a pin is a catastrophic-recovery job, and the silicon
# already provides one — the mask-ROM ISP entry pin (PB22 on ch59x, PA1 = USB
# D+ on ch57x). That path is in ROM, cannot be broken by anything OpenBoot
# flashes, and is the right tool for the job.
#
# The remaining ways into OpenBoot are: the app calling
# openboot_request_update(), or an absent/invalid boot record. A device that
# boots a healthy-but-wedged app is recovered through the ROM ISP.

# Idle auto-boot: with a valid boot record and no HELLO received, boot the app
# after this many milliseconds. 0 disables the timeout.
OB_IDLE_TIMEOUT_MS := 10000
