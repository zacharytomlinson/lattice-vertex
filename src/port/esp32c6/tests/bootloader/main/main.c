/* The maintained build selects only the vendor bootloader and partition table.
 * Runtime application images are the Rust kernel fixtures in the parent crate. */
void app_main(void) {}
