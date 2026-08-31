# Generic CH592 USB bring-up board (no product knobs).
# Recovery: mask-ROM ISP; see firmware/README.md.
override CHIP := ch592
override TRANSPORT := usb
OB_IDLE_TIMEOUT_MS := 10000
