/* Host-native mock port. Selected with -DOB_PORT_HEADER='"ob_host_port.h"'
 * plus -DOB_HOST_CH57X or -DOB_HOST_CH59X. Included by openboot_port.h
 * after the protocol header, so the OB_FAMILY / OB_FEAT / OB_APP_BASE
 * constants already exist. */
#ifndef OB_HOST_PORT_H
#define OB_HOST_PORT_H

#include <stdint.h>

#if defined(OB_HOST_CH57X)
#define OB_FLASH_APP_END   0x0003B000u
#define OB_ERASED_WORD     0xF3F9BDA9u
#define OB_CHIP_FAMILY     OB_FAMILY_CH570
#define OB_FEATURES        0x00000000u          /* F26: no live CRC */
#elif defined(OB_HOST_CH59X)
#define OB_FLASH_APP_END   0x00070000u
#define OB_ERASED_WORD     0xF3F9BDA9u   /* bench-verified on CH592A */
#define OB_CHIP_FAMILY     OB_FAMILY_CH592
#define OB_FEATURES        OB_FEAT_CRC_LIVE
#else
#error "define OB_HOST_CH57X or OB_HOST_CH59X"
#endif

#define OB_FLASH_APP_START   OB_APP_BASE
#define OB_FLASH_ERASE_BLOCK 4096u
#define OB_FLASH_WRITE_PAGE  256u
#define OB_CPU_HZ            6400000u

/* A/B slots. The real build computes these in the Makefile and injects them;
 * reproduce the same rule here (half the region, floored to the erase block)
 * so the harness exercises the geometry the firmware actually ships with. */
#define OB_SLOT_SIZE \
    ((((OB_FLASH_APP_END - OB_FLASH_APP_START) / 2u) / OB_FLASH_ERASE_BLOCK) \
     * OB_FLASH_ERASE_BLOCK)
#define OB_SLOT_A_BASE OB_FLASH_APP_START
#define OB_SLOT_B_BASE (OB_SLOT_A_BASE + OB_SLOT_SIZE)

/* Boot-request word lives in a harness variable, not at a fixed address. */
extern uint32_t ob_host_bootreq;
#define OB_BOOTREQ_ADDR ((uintptr_t)&ob_host_bootreq)

/* Route XIP through the simulated flash (bounds checks + F26 stale-read
 * poisoning of blocks modified since the last simulated reset). */
uint32_t ob_host_xip_read32(uint32_t addr);
uint8_t  ob_host_xip_read8(uint32_t addr);
#define OB_XIP_READ32(a) ob_host_xip_read32((uint32_t)(a))
#define OB_XIP_READ8(a)  ob_host_xip_read8((uint32_t)(a))

#endif /* OB_HOST_PORT_H */
