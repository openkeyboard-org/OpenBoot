/* CRC-32/ISO-HDLC, reflected poly 0xEDB88320, 4 bits per step. */
#include "crc32.h"

static const uint32_t ob_crc32_tab[16] = {
    0x00000000u, 0x1DB71064u, 0x3B6E20C8u, 0x26D930ACu,
    0x76DC4190u, 0x6B6B51F4u, 0x4DB26158u, 0x5005713Cu,
    0xEDB88320u, 0xF00F9344u, 0xD6D6A3E8u, 0xCB61B38Cu,
    0x9B64C2B0u, 0x86D3D2D4u, 0xA00AE278u, 0xBDBDF21Cu,
};

uint32_t ob_crc32_update(uint32_t state, const void *data, uint32_t len)
{
    const uint8_t *p = (const uint8_t *)data;

    while (len--) {
        state ^= *p++;
        state = (state >> 4) ^ ob_crc32_tab[state & 0x0Fu];
        state = (state >> 4) ^ ob_crc32_tab[state & 0x0Fu];
    }
    return state;
}

uint32_t ob_crc32(const void *data, uint32_t len)
{
    return ob_crc32_final(ob_crc32_update(ob_crc32_init(), data, len));
}
