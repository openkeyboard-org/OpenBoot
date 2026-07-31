/* OBP v0.1 protocol engine. Transport- and chip-agnostic; see openboot_port.h
 * for the hooks it consumes. */
#ifndef OB_BOOT_CORE_H
#define OB_BOOT_CORE_H

#include <stdint.h>

#define OB_BL_VERSION 0x000A  /* v0.10: major in high byte, minor in low */

typedef enum {
    OB_ACT_NONE  = 0,
    OB_ACT_RESET = 2,      /* main: tr_deinit() then ob_reset().
                            * 1 was OB_ACT_JUMP_APP — retired: every
                            * protocol-driven launch goes through the
                            * reset-time boot decision (single launch
                            * authority). Value kept so harness action
                            * codes stay stable. */
} ob_action_t;

void ob_core_init(void);

/* buf: candidate frame bytes (4-aligned; USB passes the whole 64 B report).
 * resp: 64 B caller buffer. *resp_len = 0 means send nothing (only for
 * frames whose length cannot be decoded). Response must be sent before
 * acting on the returned action. */
ob_action_t ob_core_handle_frame(const uint8_t *buf, uint32_t avail,
                                 uint8_t *resp, uint32_t *resp_len);

/* Nonzero once a valid HELLO has been seen this power cycle (idle
 * auto-boot is disabled from then on). */
int ob_core_session_active(void);

#endif /* OB_BOOT_CORE_H */
