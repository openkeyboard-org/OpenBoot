/* Layout for the CH592 page-erase probe firmware. Bootloader-region image
 * at the reset vector; flash routines run from RAM (.highcode). Mirrors
 * openboot.ld's structure but standalone (no generated memory.ld, no app
 * asserts). ch59x: 26 KiB SRAM. */
ENTRY(_start)

MEMORY
{
    FLASH (rx)  : ORIGIN = 0x00000000, LENGTH = 8K
    RAM   (rwx) : ORIGIN = 0x20000000, LENGTH = 26K
}

_eusrstack = ORIGIN(RAM) + LENGTH(RAM) - 16;

SECTIONS
{
    .init : { KEEP(*(SORT_NONE(.init))) } >FLASH AT>FLASH

    .text :
    {
        . = ALIGN(4);
        KEEP(*(SORT_NONE(.text.handle_reset)))
        *(.text) *(.text.*)
        *(.rodata) *(.rodata*)
        . = ALIGN(4);
    } >FLASH AT>FLASH

    .hlalign : { . = ALIGN(4); PROVIDE(_highcode_lma = .); } >FLASH AT>FLASH

    .highcode :
    {
        . = ALIGN(4);
        PROVIDE(_highcode_vma_start = .);
        *(.highcode) *(.highcode.*)
        . = ALIGN(4);
        PROVIDE(_highcode_vma_end = .);
    } >RAM AT>FLASH

    .dlalign : { . = ALIGN(4); PROVIDE(_data_lma = .); } >FLASH AT>FLASH

    .data :
    {
        . = ALIGN(4);
        PROVIDE(_data_vma = .);
        *(.data .data.*) *(.sdata .sdata.*)
        . = ALIGN(8);
        PROVIDE(__global_pointer$ = . + 0x800);
        . = ALIGN(4);
        PROVIDE(_edata = .);
    } >RAM AT>FLASH

    .bss :
    {
        . = ALIGN(4);
        PROVIDE(_sbss = .);
        *(.bss .bss.*) *(.sbss .sbss.*) *(COMMON)
        . = ALIGN(4);
        PROVIDE(_ebss = .);
    } >RAM AT>RAM

    /DISCARD/ : { *(.comment) *(.riscv.attributes) *(.debug*) *(.eh_frame*) }
}
