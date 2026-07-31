/* CRC-32/ISO-HDLC (zlib-compatible), nibble-table variant: 64 B of flash. */
#ifndef OB_CRC32_H
#define OB_CRC32_H

#include <stdint.h>

static inline uint32_t ob_crc32_init(void)            { return 0xFFFFFFFFu; }
static inline uint32_t ob_crc32_final(uint32_t state) { return state ^ 0xFFFFFFFFu; }

uint32_t ob_crc32_update(uint32_t state, const void *data, uint32_t len);
uint32_t ob_crc32(const void *data, uint32_t len);

#endif /* OB_CRC32_H */
