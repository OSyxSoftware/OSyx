/* Static OSyx user programs: loaded at 4 MiB, three W^X segments. The
   first one also maps the ELF and program headers (the kernel passes their
   address as AT_PHDR). */
ENTRY(_start)

PHDRS
{
    text   PT_LOAD FILEHDR PHDRS FLAGS(5);
    rodata PT_LOAD FLAGS(4);
    data   PT_LOAD FLAGS(6);
}

SECTIONS
{
    . = 0x400000 + SIZEOF_HEADERS;

    .text :
    {
        *(.text._start)
        *(.text .text.*)
    } :text

    . = ALIGN(4K);
    .rodata : ALIGN(4K)
    {
        *(.rodata .rodata.*)
        *(.data.rel.ro .data.rel.ro.*)
        *(.got .got.*)
    } :rodata

    . = ALIGN(4K);
    .data : ALIGN(4K)
    {
        *(.data .data.*)
    } :data

    .bss : ALIGN(16)
    {
        *(.bss .bss.*)
        *(COMMON)
    } :data

    /DISCARD/ :
    {
        *(.eh_frame .eh_frame_hdr .eh_frame.*)
        *(.note .note.*)
        *(.comment)
    }
}
