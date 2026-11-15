# MonacoKeys MK65MX Wireless keyboard module, CH592F, UART transport.
#
# Unlike the original OpenController bench board, the MK65MX routes UART1 on
# the CH592F's default PA8/RXD1 and PA9/TXD1 pins. Do not set
# OB_UART1_REMAP here: PB13 is the board's CHWAKE output and must not become
# UART1 TX.
override CHIP := ch592
override TRANSPORT := uart

# A keyboard module must not remain in the bootloader after a stray update
# request. With a blessed record, return to the application after 10 s when
# no host starts an OpenBoot session.
OB_IDLE_TIMEOUT_MS := 10000

# Refuse to boot stale or corrupted application bytes even when the boot
# record itself is still valid.
OB_BOOT_IMAGE_CRC := 1

# Run the bootloader on the 60 MHz PLL clock instead of the 6.4 MHz boot
# clock. The Bluetooth application is about 184 KB, and the boot-time and
# COMMIT image CRC take about 6 s per image at 6.4 MHz.
OB_CPU_HZ := 60000000

# A software reset can enter OpenBoot after an application has used another
# pin mapping. Explicitly release PB12/PB13 as floating inputs; PB13 is the
# MK65MX CHWAKE net and must remain undriven while the bootloader is active.
OB_UART1_ALT_PINS_HIZ := 1

# No OB_APP_END clamp: the 2.4 GHz bond is in CH59x DataFlash, outside the
# code-flash region OpenBoot can update.
