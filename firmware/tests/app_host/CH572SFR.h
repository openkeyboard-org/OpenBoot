/* Host-build stub for the vendor SFR header: just enough for
 * openboot_app.c to COMPILE off-target. openboot_request_update() is
 * compiled but never called on host (it stores to a fixed MCU address);
 * the record functions need nothing from the SDK at all. */
#ifndef OB_APP_HOST_STUB_SFR_H
#define OB_APP_HOST_STUB_SFR_H

extern volatile unsigned char ob_stub_regs[16];

#define R8_SAFE_ACCESS_SIG  (ob_stub_regs[0])
#define R8_RST_WDOG_CTRL    (ob_stub_regs[1])
#define SAFE_ACCESS_SIG0    0x00u
#define SAFE_ACCESS_SIG1    0x57u
#define SAFE_ACCESS_SIG2    0xA8u
#define RB_SOFTWARE_RESET   0x01u

#endif
