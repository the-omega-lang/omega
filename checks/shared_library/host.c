/* Host process for the shared-library check. This file owns `main` and links
 * against the host C runtime on purpose: what must be free of one is the
 * Omega library it loads, not the program that drives it.
 *
 * It is linked with `-rdynamic`. An executable's own symbols are not in its
 * dynamic table by default, and the loaded library resolves `host_secret` and
 * `host_bump` against exactly that table.
 */
#include <dlfcn.h>
#include <stdio.h>

int host_secret = 10;

void host_bump(int amount) {
	host_secret += amount;
}

int main(int argc, char **argv) {
	void *library;
	void (*run)(void);

	if (argc != 2) {
		fprintf(stderr, "usage: %s <library>\n", argv[0]);
		return 2;
	}

	/* The library writes to file descriptor 1 through the platform's own
	 * console, which knows nothing about this process's stdio. Two
	 * independent paths to one descriptor reach it in the order written only
	 * while this one holds nothing back. */
	setvbuf(stdout, NULL, _IONBF, 0);

	printf("host-before: %d\n", host_secret);

	/* `RTLD_NOW`: every reference the library makes into this process is
	 * resolved now, so an unresolvable one is reported here instead of
	 * surfacing later as a call through a null pointer. */
	library = dlopen(argv[1], RTLD_NOW);
	if (library == NULL) {
		fprintf(stderr, "dlopen: %s\n", dlerror());
		return 1;
	}

	run = (void (*)(void))dlsym(library, "omega_run");
	if (run == NULL) {
		fprintf(stderr, "dlsym: %s\n", dlerror());
		return 1;
	}

	run();
	printf("host-after: %d\n", host_secret);

	if (dlclose(library) != 0) {
		fprintf(stderr, "dlclose: %s\n", dlerror());
		return 1;
	}
	return 0;
}
