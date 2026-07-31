# Bring-up bench CH592: WCH-LinkE CDC UART wired to the UART1 alternate
# mapping (probe RX <- PB13/TXD1_, probe TX -> PB12/RXD1_).
#
# Same policy and timeout as generic-ch59x; only the UART pins differ.

# No strap: see generic-ch59x.mk (ROM ISP is the recovery path).

OB_IDLE_TIMEOUT_MS := 10000

# UART1 on PB12/PB13 via RB_PIN_UART1 (see ports/ch59x/port_ch59x.h)
OB_UART1_REMAP := 1
