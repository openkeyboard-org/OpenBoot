# Generic CH570 USB bring-up board (no product knobs).
# Recovery: mask-ROM ISP via the PA1/D+ pull-up cable; see firmware/README.md.
override CHIP := ch570
override TRANSPORT := usb
OB_IDLE_TIMEOUT_MS := 10000
