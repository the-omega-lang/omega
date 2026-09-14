/* 9 is neither a declared variant's tag nor the remainder an `else` covers. */
void corrupt_signal_tag(unsigned char *slot) { *slot = 9; }
