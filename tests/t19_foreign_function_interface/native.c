/* The C side of this case's ABI coverage.
 *
 * Compiled freestanding and linked into the test executable, exactly like the
 * Omega objects: what is under test is Omega's C calling convention, not a C
 * library. Nothing here calls libc, so the program still links with no C
 * runtime at all -- which is the point. `stdarg.h` is a compiler header and
 * is available to a freestanding translation unit.
 */
#include <stdarg.h>

int t19_abs(int x) {
	return x < 0 ? -x : x;
}

/* A pointer-returning entry point: the practical FFI boundary is scalars and
 * pointers, and the Omega side writes through this one. */
static unsigned char t19_storage[64];

unsigned char *t19_scratch(void) {
	return t19_storage;
}

/* An integer variadic tail. The second entry point exists so the Omega side
 * can declare the same shape under `sysv64` without two declarations
 * competing for one linker symbol. */
static int sum_ints(int count, va_list args) {
	int total = 0;
	for (int i = 0; i < count; i++) {
		total += va_arg(args, int);
	}
	return total;
}

int t19_sum_ints(int count, ...) {
	va_list args;
	va_start(args, count);
	int total = sum_ints(count, args);
	va_end(args);
	return total;
}

int t19_sum_ints_sysv(int count, ...) {
	va_list args;
	va_start(args, count);
	int total = sum_ints(count, args);
	va_end(args);
	return total;
}

/* A floating-point variadic tail. On System V x86-64 the callee only saves
 * the vector registers when the caller reports how many it used in `%al`, so
 * a caller that gets that wrong reads garbage here. The result is scaled to
 * an integer so the case asserts the transferred values without depending on
 * float formatting. */
int t19_sum_doubles_scaled(int count, ...) {
	va_list args;
	double total = 0.0;
	va_start(args, count);
	for (int i = 0; i < count; i++) {
		total += va_arg(args, double);
	}
	va_end(args);
	return (int)(total * 100.0);
}

/* A mixed tail: `ints` integer arguments followed by `doubles` floating-point
 * ones, so one call exercises both variadic register classes. */
int t19_mix_scaled(int ints, int doubles, ...) {
	va_list args;
	int whole = 0;
	double fraction = 0.0;
	va_start(args, doubles);
	for (int i = 0; i < ints; i++) {
		whole += va_arg(args, int);
	}
	for (int i = 0; i < doubles; i++) {
		fraction += va_arg(args, double);
	}
	va_end(args);
	return whole * 100 + (int)(fraction * 100.0);
}
