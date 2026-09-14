/* The tag is the first byte of an `enum Signal` value, whose storage the
 * caller already initialized. Neither 3 nor anything above 1 is a declared
 * variant. */
void corrupt_signal_tag(unsigned char *slot) { *slot = 3; }
