# Generic CH570/CH572 bring-up board.
#
# A board .mk maps hardware choices onto -D knobs (the Makefile does the
# quoting and injection). Real boards get their own file next to this one and
# are selected with BOARD=<name> on the command line.

# No stay-in-bootloader strap: OpenBoot does not have one. CH570/CH572 do not
# need it — their mask-ROM ISP entry pin is PA1, which is also USB D+ (UDP;
# EVT/PUB/CH572SCH.pdf wires the DOWNLOAD button's VCC through R12 to PA1), so
# a device can be forced into the ROM bootloader through its own USB connector
# with a pull-UP on D+ at power-on, no dedicated GPIO spent.
#
# Consequence, and it is deliberate: with a USB image (which clears
# RB_PIN_DEBUG_EN and so kills SWD), the ways into the bootloader are the app
# calling openboot_request_update(), an absent/invalid boot record, or the
# ROM-ISP pull-up cable. That cable is the sole recovery for an app that boots
# but cannot request an update. See firmware/README.md.

# Idle auto-boot: with a valid boot record and no HELLO received, boot the app
# after this many milliseconds. 0 disables the timeout. Real milliseconds off
# SysTick — this used to be a poll count that measured ~273 s here.
OB_IDLE_TIMEOUT_MS := 10000
