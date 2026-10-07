/* Standalone nRF52840 image: no SoftDevice or bootloader reserved here. */
MEMORY
{
    FLASH : ORIGIN = 0x00000000, LENGTH = 1M
    RAM   : ORIGIN = 0x20000000, LENGTH = 256K
}

/* Separate exception/main stack; task stacks are fixed storage in .bss. */
_stack_start = ORIGIN(RAM) + LENGTH(RAM);
_stack_end = _stack_start - 4K;
