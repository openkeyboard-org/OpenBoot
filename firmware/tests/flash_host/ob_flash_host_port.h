/* Host-native mock port for the flash driver unit tests. Selected with
 * -DOB_PORT_HEADER='"ob_flash_host_port.h"' plus -DOB_HOST_CH57X or
 * -DOB_HOST_CH59X. Included by openboot_port.h after the protocol header.
 *
 * Substitutes every hardware seam the driver has: the OB_FL_* register
 * accessors become recording mock calls (flash_ch5xx.c's defaults are
 * skipped via OB_FL_REGS_MOCKED), the safe-access helpers become events,
 * and OB_CPU_HZ is tiny so the WIP-timeout path completes in ~100 mock
 * iterations instead of 160k. */
#ifndef OB_FLASH_HOST_PORT_H
#define OB_FLASH_HOST_PORT_H

#include <stdint.h>

#if defined(OB_HOST_CH57X)
#define OB_FLASH_APP_END   0x0003C000u
#define OB_CHIP_FAMILY     OB_FAMILY_CH570
#define OB_FEATURES        0x00000000u
/* CH572SFR.h: two-bit code write-enable field. */
#define RB_ROM_CODE_OFS    0x10
#define RB_ROM_CTRL_EN     0x20
#define RB_ROM_DATA_WE     0x40
#define RB_ROM_CODE_WE     0xC0
#elif defined(OB_HOST_CH59X)
#define OB_FLASH_APP_END   0x00070000u
#define OB_CHIP_FAMILY     OB_FAMILY_CH592
#define OB_FEATURES        OB_FEAT_CRC_LIVE
/* CH592SFR.h: separate data / code write-enable bits. */
#define RB_ROM_CODE_OFS    0x10
#define RB_ROM_CTRL_EN     0x20
#define RB_ROM_DATA_WE     0x40
#define RB_ROM_CODE_WE     0x80
#else
#error "define OB_HOST_CH57X or OB_HOST_CH59X"
#endif

#define OB_ERASED_WORD       0xF3F9BDA9u
#define OB_FLASH_APP_START   OB_APP_BASE
/* Mirror the real ch59x port: OB_FLASH_PAGE_ERASE (passed as -D by the
 * ch59x_pageerase .so build) lowers the erase block to 256 so the driver's
 * OB_FL_ERASE_OP derivation and the mock's sequencing match firmware. */
#if defined(OB_FLASH_PAGE_ERASE) && OB_FLASH_PAGE_ERASE
#define OB_FLASH_ERASE_BLOCK 256u
#else
#define OB_FLASH_ERASE_BLOCK 4096u
#endif
#define OB_FLASH_WRITE_PAGE  256u
#define OB_CPU_HZ            4000u   /* OB_FL_WAIT_ITERS = 100 */
#define OB_STK_CNT64         0

#define OB_SLOT_SIZE \
    ((((OB_FLASH_APP_END - OB_FLASH_APP_START) / 2u) / OB_FLASH_ERASE_BLOCK) \
     * OB_FLASH_ERASE_BLOCK)
#define OB_SLOT_A_BASE OB_FLASH_APP_START
#define OB_SLOT_B_BASE (OB_SLOT_A_BASE + OB_SLOT_SIZE)

extern uint32_t ob_host_bootreq;
#define OB_BOOTREQ_ADDR ((uintptr_t)&ob_host_bootreq)

/* ---- driver seams ------------------------------------------------------ */
#define OB_FL_HIGHCODE(fn)              /* no ELF sections on the host */

#define OB_FL_REGS_MOCKED 1
uint8_t  ob_flmock_ctrl_rd(void);
void     ob_flmock_ctrl_wr(uint8_t v);
uint8_t  ob_flmock_data8_rd(void);
void     ob_flmock_data8_wr(uint8_t v);
uint32_t ob_flmock_data32_rd(void);
void     ob_flmock_data32_wr(uint32_t v);
uint8_t  ob_flmock_glob_rd(void);
void     ob_flmock_glob_wr(uint8_t v);
void     ob_flmock_nop(void);
void     ob_flmock_safe(int on);

#define OB_FL_CTRL_RD()    ob_flmock_ctrl_rd()
#define OB_FL_CTRL_WR(v)   ob_flmock_ctrl_wr((uint8_t)(v))
#define OB_FL_DATA8_RD()   ob_flmock_data8_rd()
#define OB_FL_DATA8_WR(v)  ob_flmock_data8_wr((uint8_t)(v))
#define OB_FL_DATA32_RD()  ob_flmock_data32_rd()
#define OB_FL_DATA32_WR(v) ob_flmock_data32_wr((uint32_t)(v))
#define OB_FL_GLOB_RD()    ob_flmock_glob_rd()
#define OB_FL_GLOB_WR(v)   ob_flmock_glob_wr((uint8_t)(v))
#define OB_FL_NOP()        ob_flmock_nop()

static inline void sys_safe_access_enable(void)  { ob_flmock_safe(1); }
static inline void sys_safe_access_disable(void) { ob_flmock_safe(0); }

#endif /* OB_FLASH_HOST_PORT_H */
