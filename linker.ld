ENTRY(_start)

SECTIONS
{
    /* Start the kernel at the 1MB mark */
    . = 1M;

    /* Combine EVERYTHING into the standard .text section to force rust-lld
       to generate exactly one PT_LOAD segment. If we create a custom section
       like .veilos_kernel, LLD falls back to splitting segments by permissions.
       Using .text overrides the default linker script's behavior perfectly. */
    .text : ALIGN(4K) {
        *(.text .text.*)
        *(.rodata .rodata.*)
        *(.data .data.*)
        *(.bss .bss.*)
        *(COMMON)
        *(.got .got.*)
        *(.data.rel.ro .data.rel.ro.*)
    }

    /DISCARD/ : {
        *(.eh_frame)
        *(.comment)
    }
}