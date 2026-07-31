/*
 * OpenBoot application companion API.
 *
 * Compile openboot_app.c into the application with exactly one of
 * -DOPENBOOT_CHIP_CH57X (CH570/CH572) or -DOPENBOOT_CHIP_CH59X
 * (CH591/CH592) and the matching SDK StdPeriphDriver/inc on the include
 * path. See README.md in this directory for linker requirements (app base
 * 0x2000, reserved top 16 bytes of RAM, ch59x DataFlash reservation).
 */
#ifndef OPENBOOT_APP_H
#define OPENBOOT_APP_H

#include "../../protocol/openboot_protocol.h"

#ifdef __cplusplus
extern "C" {
#endif

/*
 * Read the boot record the bootloader stored at COMMIT (CH57x: code flash
 * 0x3B000; CH59x: DataFlash offset 0x7000 — needs libISP592 linked).
 * Returns 0 and fills *out when a record with a valid magic was read;
 * nonzero otherwise. The caller may additionally check rec_crc32
 * (CRC-32/ISO-HDLC over the first 12 bytes) if it has a CRC32 handy.
 */
int openboot_get_record(ob_boot_record_t *out);

/*
 * Request a bootloader (IAP) session: writes OB_BOOTREQ_MAGIC to the
 * reserved top-of-RAM word and performs a software reset. Never returns.
 * No flash is touched on this path. Requires the application linker to
 * keep its stack top at or below OB_BOOTREQ_ADDR_* (see README.md) so the
 * magic survives until the bootloader reads it.
 */
void openboot_request_update(void) __attribute__((noreturn));

#ifdef __cplusplus
}
#endif

#endif /* OPENBOOT_APP_H */
