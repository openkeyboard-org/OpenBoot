/* Bench A/B witness image.
 *
 * CLEARS the other slot's witness word, writes its own distinctive constant,
 * arms the OpenBoot RAM boot request, and spins.
 *
 * Clearing the other word is what makes the evidence unambiguous: exactly one
 * witness is ever set, and it names the image that ran most recently. Without
 * it, a marker left by an earlier run can survive a power cut intact (SRAM
 * here decays unpredictably - sometimes to zero, sometimes not at all), so
 * "B's marker is present" would not prove B just ran.
 *
 * Arming the boot request is the only way back into the bootloader once an
 * app is installed: SWD RAM writes do not stick through minichlink. It also
 * exercises app -> bootloader entry.
 */
    .section .text
    .globl _start
_start:
    li   a0, OTHER_ADDR
    sw   zero, 0(a0)
    li   a0, WITNESS_ADDR
    li   a1, WITNESS_VALUE
    sw   a1, 0(a0)
    li   a0, BOOTREQ_ADDR
    li   a1, 0xB007CA11
    sw   a1, 0(a0)
1:  j    1b
