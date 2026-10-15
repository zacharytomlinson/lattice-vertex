/* XIAO nRF52840 stock S140 v7 / UF2 application layout.
 * See Seeed-Studio/Adafruit_nRF52_Arduino:
 * cores/nRF5/linker/nrf52840_s140_v7.ld.
 * Keep MBR/SoftDevice below 0x27000 and storage/bootloader above 0xED000.
 * Lattice never enables the SoftDevice, so the application owns all RAM.
 */
MEMORY
{
    FLASH : ORIGIN = 0x00027000, LENGTH = 0xED000 - 0x27000
    RAM   : ORIGIN = 0x20000000, LENGTH = 256K
}

_stack_start = ORIGIN(RAM) + LENGTH(RAM);
_stack_end = _stack_start - 4K;
