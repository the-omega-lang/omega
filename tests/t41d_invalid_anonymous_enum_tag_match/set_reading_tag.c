/* An anonymous enum stores a `u32` tag first; the payload behind it is left
 * exactly as the caller initialized it. */
void set_reading_tag(unsigned int *slot, unsigned int tag) { *slot = tag; }
