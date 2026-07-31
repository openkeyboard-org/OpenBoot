/* Reset-time boot decision: runs before transport init, decides whether to
 * stay in the bootloader or jump to the app. */
#ifndef OB_BOOT_DECISION_H
#define OB_BOOT_DECISION_H

#include "../../protocol/openboot_protocol.h"

/* Returns only when staying in the bootloader; otherwise calls
 * ob_jump_app(). Order: strap pin (debounced), RAM boot-request magic
 * (cleared when seen), boot record, optional full image CRC
 * (OB_BOOT_IMAGE_CRC), erased first app word. */
void ob_boot_decide(void);

/* Record + image checks only (no strap/bootreq, no side effects). Used by
 * the pre-HELLO idle timeout and the explicit-BOOT validation. */
int ob_boot_app_valid(void);

/* The canonical record-validity check (PROTOCOL.md section 9.1): read
 * success, magic, rec_crc32, and always-on img_len geometry. Fills *rec on
 * success; the ONLY definition of "valid record" in the tree. */
int ob_record_load(ob_boot_record_t *rec);

#endif /* OB_BOOT_DECISION_H */
