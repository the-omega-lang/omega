/* `kind` is the holder's first field and its tag is the enum's first field. */
void set_kind_tag(unsigned char *slot, unsigned char tag) { *slot = tag; }
