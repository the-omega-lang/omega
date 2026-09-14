/* `Result<i32, Narrow>` is [u32 tag][payload]; the anonymous error enum sits
 * in the payload and carries a `u32` tag of its own. The payload starts at
 * the value's alignment boundary after the outer tag, which for these
 * pointer-free members is offset 4. */
void set_error_tag(unsigned int *slot, unsigned int tag) { slot[1] = tag; }
