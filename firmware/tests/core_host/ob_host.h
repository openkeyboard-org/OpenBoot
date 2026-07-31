/* C API exported to the pytest suite via ctypes (see ob_native.py). */
#ifndef OB_HOST_H
#define OB_HOST_H

#include <stdint.h>
#include "../../../protocol/openboot_protocol.h"

void     host_reset(void);           /* factory-fresh chip + core init */
void     host_power_cycle(void);     /* keep flash/record; RAM + XIP coherence lost */
void     host_frame(const uint8_t *in, uint32_t inlen,
                    uint8_t *out, uint32_t *outlen, int32_t *action);
void     host_flash_read(uint32_t addr, uint8_t *buf, uint32_t len); /* true content */
void     host_record_raw(uint8_t out[OB_BOOT_RECORD_SIZE]);
void     host_set_record_raw(const uint8_t in[OB_BOOT_RECORD_SIZE]);
void     host_set_fail_after(int32_t n);   /* n mutating ops then power cut; -1 = never */
uint32_t host_violations(void);            /* port-contract breaches seen by the sim */
uint32_t host_op_total(void);              /* mutating flash ops attempted since reset */
void     host_set_bootreq(uint32_t v);
uint32_t host_get_bootreq(void);
void     host_set_bootpin(int32_t v);
void     host_set_silicon_app_end(uint32_t v);
void     host_set_f26(int32_t v);          /* default: on for ch57x builds */
int32_t  host_boot_decide_result(void);    /* 0 = jump app, 1 = stay */

#endif /* OB_HOST_H */
