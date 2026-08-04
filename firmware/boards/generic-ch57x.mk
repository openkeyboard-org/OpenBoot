# Generic CH570/CH572 bring-up board.
#
# A board .mk maps hardware choices onto -D knobs (the Makefile does the
# quoting and injection). Real boards get their own file next to this one and
# are selected with BOARD=<name> on the command line.

# NO stay-in-bootloader strap on this family, deliberately.
#
# CH570/CH572 do not need one: their mask-ROM ISP entry pin is PA1, which is
# also USB D+ (UDP) — see EVT/PUB/CH572SCH.pdf, where the DOWNLOAD button
# wires VCC through R12 to PA1. So a device can be forced into the ROM
# bootloader through its own USB connector with a pull-UP on D+ at power-on,
# and no dedicated GPIO has to be spent on a strap. CH59x also relies on its
# dedicated mask-ROM ISP pin rather than defining an OpenBoot strap.
#
# It would not fit the parts anyway: the CH570Q/CH572Q package (DFN10X3)
# bonds out only PA0, PA1, PA2, PA3, PA7 (datasheet Table 1-2). The former
# default here was PA4, which those packages do not have — so it could never
# assert, silently. Check package bonding, not just the die, before setting
# this on any ch57x board.
#
# Consequence, and it is deliberate: with no strap and a USB image (which
# clears RB_PIN_DEBUG_EN and so kills SWD), the ways into the bootloader are
# the app calling openboot_request_update(), an absent/invalid boot record,
# or the ROM-ISP pull-up cable. That cable is the sole recovery for an app
# that boots but cannot request an update. See firmware/README.md.
#
# OB_BOOT_PIN_MASK intentionally unset -> ob_bootpin_asserted() compiles to
# `return 0` (~82 bytes saved). A board whose package DOES expose a spare pin
# may set it; ch57x has port A only, so OB_BOOT_PIN_PORT_B must stay 0.

# Idle auto-boot: with a valid boot record and no HELLO received, boot the app
# after this many milliseconds. 0 disables the timeout. Real milliseconds off
# SysTick — this used to be a poll count that measured ~273 s here.
OB_IDLE_TIMEOUT_MS := 10000
