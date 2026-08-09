/*
 * OpenBoot linker script, all supported families: 8 KiB bootloader at the
 * reset vector. Derived from the proven OpenDongle stage1.ld.
 *
 * The MEMORY block and OB_BOOTREQ_ADDR come from generated memory.ld in the
 * build directory. _eusrstack derives from the RAM geometry; OB_BOOTREQ_ADDR
 * is extracted from protocol/openboot_protocol.h, and the ASSERT below is
 * the one cross-check keeping the Makefile's RAM_SIZE and the hand-written
 * protocol constant in lockstep.
 *
 * No vector table: startup_ch5xx.S is vectorless (mtvec -> ob_trap).
 * Top 16 bytes of RAM are reserved for the app->bootloader request magic
 * and must stay outside the stack.
 */
ENTRY( _start )

INCLUDE memory.ld

_eusrstack = ORIGIN(RAM) + LENGTH(RAM) - 16;
__stack_size = 0x800;

SECTIONS
{
    .init :
    {
        . = ALIGN(4);
        KEEP(*(SORT_NONE(.init)))
        . = ALIGN(4);
    } >FLASH AT>FLASH

    .text :
    {
        . = ALIGN(4);
        KEEP(*(SORT_NONE(.text.handle_reset)))
        *(.text)
        *(.text.*)
        *(.rodata)
        *(.rodata*)
        *(.sdata2.*)
        *(.gnu.linkonce.t.*)
        . = ALIGN(4);
    } >FLASH AT>FLASH

    .hlalign :
    {
        . = ALIGN(4);
        PROVIDE(_highcode_lma = .);
    } >FLASH AT>FLASH

    /* libISP (.highcode.FLASH_*) and clock init: must execute from RAM */
    .highcode :
    {
        . = ALIGN(4);
        PROVIDE(_highcode_vma_start = .);
        *(.highcode)
        *(.highcode.*)
        . = ALIGN(4);
        PROVIDE(_highcode_vma_end = .);
    } >RAM AT>FLASH

    .dlalign :
    {
        . = ALIGN(4);
        PROVIDE(_data_lma = .);
    } >FLASH AT>FLASH

    .data :
    {
        . = ALIGN(4);
        PROVIDE(_data_vma = .);
        *(.gnu.linkonce.r.*)
        *(.data .data.*)
        *(.gnu.linkonce.d.*)
        . = ALIGN(8);
        PROVIDE(__global_pointer$ = . + 0x800);
        *(.sdata .sdata.*)
        *(.gnu.linkonce.s.*)
        . = ALIGN(8);
        *(.srodata.cst16)
        *(.srodata.cst8)
        *(.srodata.cst4)
        *(.srodata.cst2)
        *(.srodata .srodata.*)
        . = ALIGN(4);
        PROVIDE(_edata = .);
    } >RAM AT>FLASH

    .bss :
    {
        . = ALIGN(4);
        PROVIDE(_sbss = .);
        *(.sbss*)
        *(.gnu.linkonce.sb.*)
        *(.bss*)
        *(.gnu.linkonce.b.*)
        *(COMMON*)
        . = ALIGN(4);
        PROVIDE(_ebss = .);
    } >RAM

    PROVIDE(_end = _ebss);
    PROVIDE(end = _end);

    ASSERT(_end + __stack_size <= _eusrstack,
           "OpenBoot: less than 0x800 bytes left for the stack")
    ASSERT(_eusrstack == OB_BOOTREQ_ADDR,
           "OpenBoot: _eusrstack must equal the family's OB_BOOTREQ_ADDR from protocol/openboot_protocol.h")
}
