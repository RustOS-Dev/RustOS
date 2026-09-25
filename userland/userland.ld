/* Link RustOS user programs at 4 MiB (static, non-PIE). */
ENTRY(_start)

SECTIONS {
    . = 0x400000;
    .text : ALIGN(4096) { *(.text._start) *(.text .text.*) }
    .rodata : ALIGN(4096) { *(.rodata .rodata.*) }
    .data.rel.ro : ALIGN(4096) { *(.data.rel.ro .data.rel.ro.*) *(.got .got.*) }
    .data : ALIGN(4096) { *(.data .data.*) }
    .bss : ALIGN(4096) { *(.bss .bss.*) *(COMMON) }
    /DISCARD/ : { *(.eh_frame*) *(.note .note.*) *(.comment) }
}
