/* Minimal vectorless CH592 (ch59x) startup for the page-erase probe.
 * Mirrors firmware/ports/startup_ch5xx.S's proven reset idiom — gp/sp, the
 * .highcode LMA->VMA copy, bss zero, the ch59x microarchitecture CSRs
 * (corecfgr 0x1f, intsyscr 0x3) — then calls main(). No transport, no
 * boot decision, no app: this image exists only to run probe_fw.c's main. */
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
    la      sp, _eusrstack

    /* copy the RAM-resident flash routines (.highcode) */
    la      a0, _highcode_lma
    la      a1, _highcode_vma_start
    la      a2, _highcode_vma_end
    bgeu    a1, a2, 2f
1:  lw      t0, (a0)
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
1:  lw      t0, (a0)
    sw      t0, (a1)
    addi    a0, a0, 4
    addi    a1, a1, 4
    bltu    a1, a2, 1b
2:
    /* zero .bss */
    la      a0, _sbss
    la      a1, _ebss
    bgeu    a0, a1, 2f
1:  sw      zero, (a0)
    addi    a0, a0, 4
    bltu    a0, a1, 1b
2:
    li      t0, 0x1f            /* ch59x corecfgr */
    csrw    0xbc0, t0
    li      t0, 0x3             /* intsyscr */
    csrw    0x804, t0
    li      t0, 0x1800
    csrw    mstatus, t0
    la      t0, trap
    csrw    mtvec, t0
    call    main
trap:
1:  j       1b
