MEMORY
{
  /* Leave 16 KiB for the Wio Terminal's default bootloader. */
  FLASH (rx)  : ORIGIN = 0x00000000 + 16K, LENGTH = 512K - 16K
  RAM   (rxw) : ORIGIN = 0x20000000, LENGTH = 192K
}

_stack_start = ORIGIN(RAM) + LENGTH(RAM);

/* The ATSAMD51's vector table (16 exceptions + 137 interrupts, 4 bytes each)
   ends 4 bytes shy of an 8-byte boundary, but .text contains input sections
   with 8-byte alignment. Round the default `_stext` (end of vector table) up
   so rust-lld doesn't warn about a misaligned .text section. */
_stext = ALIGN(ADDR(.vector_table) + SIZEOF(.vector_table), 8);

