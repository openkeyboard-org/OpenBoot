# Bring-up bench CH592: WCH-LinkE CDC UART wired to the UART1 alternate
# mapping (probe RX <- PB13/TXD1_, probe TX -> PB12/RXD1_).
#
# Same timeout as the generic ch592 bring-up boards; only the UART pins differ.
# Recovery: mask-ROM ISP; see firmware/README.md.
override CHIP := ch592
override TRANSPORT := uart

OB_IDLE_TIMEOUT_MS := 10000

# UART1 on PB12/PB13 via RB_PIN_UART1 (see ports/ch59x/port_ch59x.h)
OB_UART1_REMAP := 1
