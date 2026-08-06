/*
 * OpenBoot application companion API.
 *
 * Compile openboot_app.c into the application with exactly one of
 * -DOPENBOOT_CHIP_CH57X (CH570/CH572) or -DOPENBOOT_CHIP_CH59X
 * (CH591/CH592), plus -DOPENBOOT_SLOT_BASE / -DOPENBOOT_SLOT_SIZE naming
 * the slot this build is linked for, and the matching SDK
 * StdPeriphDriver/inc on the include path. See README.md in this directory
 * for the linker requirements (linked at the slot base, top 16 bytes of
 * RAM reserved for the boot request).
 */
#ifndef OPENBOOT_APP_H
#define OPENBOOT_APP_H

#include "../../protocol/openboot_protocol.h"

#ifdef __cplusplus
extern "C" {
#endif

/*
 * Read this slot's boot record — the one the bootloader wrote when it
 * committed this image. It lives in code flash on BOTH families, at the
 * start of the slot's final erase block
 * (OPENBOOT_SLOT_BASE + OPENBOOT_SLOT_SIZE - 4096), so this is a plain
 * memory read: no ROM API and no libISP on either chip.
 *
 * Returns 0 and fills *out only when the record passes the SAME validity
 * rule the bootloader applies before booting: magic, rec_crc32
 * (CRC-32/ISO-HDLC over the first OB_RECORD_CRC_LEN = 28 bytes), reserved
 * bytes zero, generation nonzero, and img_len a nonzero 4-byte multiple
 * that fits the slot. Nonzero otherwise; *out then holds the raw bytes
 * read, for diagnostics.
 */
int openboot_get_record(ob_boot_record_t *out);

/*
 * The validity rule alone, for OB_BOOT_RECORD_SIZE (32) bytes obtained some
 * other way — the other slot's record, or one carried in an update payload.
 * Takes a byte pointer of ANY alignment, which is why it is not typed
 * ob_boot_record_t*: payload buffers are often byte-aligned, and the fields
 * are read via an internal aligned copy rather than through the caller's
 * pointer. Nonzero = valid by the bootloader's definition, except the
 * slot-capacity bound, which needs a slot: img_len is checked against THIS
 * build's slot capacity (OPENBOOT_SLOT_SIZE - 4096).
 */
int openboot_record_valid(const void *rec_bytes);

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
