#ifndef OPENBOOT_PORT_CH5XX_PRIVATE_H
#define OPENBOOT_PORT_CH5XX_PRIVATE_H

#include <stdint.h>

/* The small family-specific part behind the shared CH5xx port. Clock setup
 * runs from RAM because it changes flash timing while XIP is active. */
void ob_family_clock_init(void) __attribute__((section(".highcode")));
void ob_family_read_uid(uint32_t buf[4]);

#endif /* OPENBOOT_PORT_CH5XX_PRIVATE_H */
