/*
 * OpenBoot startup, CH591/CH592 (Qingke V4C, rv32imac).
 *
 * Vectorless: mtvec points at ob_trap (direct mode) and MIE stays 0 — the
 * bootloader never enables interrupts, so no vector table exists. This
 * also removes the EVT-IAP bug class where the vector table is linked to
 * RAM but never copied.
 */

    .section    .init, "ax", @progbits
    .global     _start
    .align      1
_start:
    j       handle_reset

    .section    .text.handle_reset, "ax", @progbits
    .align      1
handle_reset:
.option push
.option norelax
    la      gp, __global_pointer$
.option pop
    /* _eusrstack = top of RAM - 16: the boot-request magic word above the
     * stack must survive every push */
    la      sp, _eusrstack

    /* copy .highcode (libISP + clock init must run from RAM) */
    la      a0, _highcode_lma
    la      a1, _highcode_vma_start
    la      a2, _highcode_vma_end
    bgeu    a1, a2, 2f
1:
    lw      t0, (a0)
    sw      t0, (a1)
    addi    a0, a0, 4
    addi    a1, a1, 4
    bltu    a1, a2, 1b
2:
    /* copy .data */
    la      a0, _data_lma
    la      a1, _data_vma
    la      a2, _edata
    bgeu    a1, a2, 2f
1:
    lw      t0, (a0)
    sw      t0, (a1)
    addi    a0, a0, 4
    addi    a1, a1, 4
    bltu    a1, a2, 1b
2:
    /* zero .bss */
    la      a0, _sbss
    la      a1, _ebss
    bgeu    a0, a1, 2f
1:
    sw      zero, (a0)
    addi    a0, a0, 4
    bltu    a0, a1, 1b
2:
    /* per-family microarchitecture CSRs (EVT startup_CH592.S values):
     * 0xbc0 corecfgr=0x1f (pipeline/prefetch), 0x804 intsyscr=0x3 */
    li      t0, 0x1f
    csrw    0xbc0, t0
    li      t0, 0x3
    csrw    0x804, t0

    /* MPP=machine, MIE=0/MPIE=0 — interrupts never enabled */
    li      t0, 0x1800
    csrw    mstatus, t0

    /* direct mode: bits[1:0]=0, all traps park at ob_trap */
    la      t0, ob_trap
    csrw    mtvec, t0

    call    main
1:
    j       1b

    .section    .text.ob_trap, "ax", @progbits
    .global     ob_trap
    .align      2
ob_trap:
    j       ob_trap
