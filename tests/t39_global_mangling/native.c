/* The C side of this case's global-symbol coverage.
 *
 * Compiled freestanding and linked into the test executable like every other
 * conformance case: what is under test is which linker name each Omega global
 * chose, so C must be able to name that storage directly. Only globals whose
 * Omega binding is `mut` are written here.
 */

/* Defined here, bound in Omega as `outside_sym` through
 * `@symbol(name = "symbol_from_outside")`. */
int symbol_from_outside = 4;

/* Defined in Omega, all named by the symbol its annotation selected. */
extern int unmangled_symbol_with_default_value;
extern int shared_counter;
extern int inferred_scale;
extern int plain_flag;

int gm_default_value(void) {
	return unmangled_symbol_with_default_value;
}

int gm_scale(void) {
	return inferred_scale;
}

int gm_counter(void) {
	return shared_counter;
}

void gm_bump_counter(int amount) {
	shared_counter += amount;
}

int gm_flag(void) {
	return plain_flag;
}

void gm_set_flag(int value) {
	plain_flag = value;
}
