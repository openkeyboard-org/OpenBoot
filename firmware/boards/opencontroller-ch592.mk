# OpenController keyboard wireless module, CH592F, UART transport.
#
# The module has no host-facing USB: its only wired link is UART1 on the
# PB12/PB13 alternate mapping, shared with the application's QMK-host
# protocol (probe RX <- PB13/TXD1_, probe TX -> PB12/RXD1_).
#
# Recovery: mask-ROM ISP; see firmware/README.md.
override CHIP := ch592
override TRANSPORT := uart

# Idle auto-boot: a keyboard module must never sit dead in the bootloader.
# With a blessed record, a spurious enter-bootloader (stray A6 81 from the
# host MCU) costs ~10 s off-air and the application boots back on its own.
OB_IDLE_TIMEOUT_MS := 10000

# UART1 on PB12/PB13 via RB_PIN_UART1 (see ports/ch59x/port_ch59x.h)
OB_UART1_REMAP := 1

# Full image-CRC check at every boot. The module is SWD-reflashed on the
# bench; a stale-but-valid boot record must refuse to launch mismatched
# bytes rather than silently run them.
OB_BOOT_IMAGE_CRC := 1

# No OB_APP_END clamp: full-size slots. The module's 2.4G bond record lives
# in CH59x DataFlash (logical 0x4000), which OBP cannot reach by
# construction (code-flash-only protocol).
