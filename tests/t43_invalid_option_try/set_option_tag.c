/* `Option<i32>` stores its implicit `u32` tag first. */
void set_option_tag(unsigned int *slot, unsigned int tag) { *slot = tag; }
