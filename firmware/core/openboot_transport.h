/*
 * Transport contract. Exactly one transport (usb_transport.c or
 * uart_transport.c) is linked per build; direct symbols, no vtable.
 *
 * Transports move candidate frame bytes; ALL validation (len byte, CRC,
 * opcode, state) happens in the core so it exists in exactly one place.
 */
#ifndef OPENBOOT_TRANSPORT_H
#define OPENBOOT_TRANSPORT_H

#include <stdint.h>

/* The main loop sleeps this long between tr_poll calls; transports may
 * convert milliseconds to poll counts with this (there is no other time
 * source — the bootloader never enables timers or interrupts). */
#define OB_POLL_INTERVAL_US 20

void tr_init(void);

/* Non-blocking. Returns NULL when no candidate frame is pending, else a
 * pointer to a 4-byte-aligned buffer of *avail bytes:
 *  - USB: the whole 64-byte OUT report (core trims using the len byte).
 *  - UART: header + payload + CRC as framed by the SOF/len parser
 *    (the parser silently drops frames whose len byte exceeds
 *    OB_MAX_PAYLOAD and re-hunts; a >OB_UART_INTERBYTE_MS mid-frame gap
 *    resets the parser only, never the session).
 * The buffer stays valid until the next tr_poll/tr_send call. */
const uint8_t *tr_poll(uint32_t *avail);

/* Blocking send of one complete logical frame (core built it, CRC
 * included). USB: zero-pads to a 64-byte IN report. UART: writes SOF then
 * the frame. */
void tr_send(const uint8_t *frame, uint32_t len);

/* Quiesce before jumping to the app: USB resets the SIE and drops the DP
 * pull-up then waits 10 ms; UART drains the TX FIFO plus one character
 * time. */
void tr_deinit(void);

#endif /* OPENBOOT_TRANSPORT_H */
