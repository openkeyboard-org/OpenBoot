/* Shared vectorless OpenBoot startup for the supported QingKe RV32 cores. */

#ifndef OB_STARTUP_CORECFGR
#error "build must define OB_STARTUP_CORECFGR"
#endif
#ifndef OB_STARTUP_WRITE_BC1
#error "build must define OB_STARTUP_WRITE_BC1"
#endif

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
    /* Top 16 bytes of RAM hold the application-to-bootloader request. */
    la      sp, _eusrstack

    /* Copy RAM-resident libISP and family clock setup. */
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
    la      a0, _sbss
    la      a1, _ebss
    bgeu    a0, a1, 2f
1:
    sw      zero, (a0)
    addi    a0, a0, 4
    bltu    a0, a1, 1b
2:
    /* per-family microarchitecture CSRs, values injected by the Makefile
     * from the WCH EVT startup files: 0xbc0 corecfgr (pipeline/prefetch),
     * 0x804 intsyscr; only ch57x writes 0xbc1=1 */
    li      t0, OB_STARTUP_CORECFGR
    csrw    0xbc0, t0
    li      t0, 0x3
    csrw    0x804, t0
#if OB_STARTUP_WRITE_BC1
    li      t0, 0x1
    csrw    0xbc1, t0
#endif

    li      t0, 0x1800
    csrw    mstatus, t0
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
