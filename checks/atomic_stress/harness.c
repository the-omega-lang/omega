/* Host thread harness for the platform's atomic glue. This file owns `main`
 * and the threads; every atomic operation happens on the Omega side. It links
 * against the host C runtime on purpose -- what must be free of one is the
 * platform implementation it calls, not the test that drives it. */
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>

uint32_t omega_fetch_add_u32(uint32_t *slot, uint32_t value);
void omega_cas_increment_u32(uint32_t *slot);
uint16_t omega_fetch_add_u16(uint16_t *slot, uint16_t value);
uint64_t omega_fetch_add_u64(uint64_t *slot, uint64_t value);
uint8_t omega_fetch_or_u8(uint8_t *slot, uint8_t value);
int32_t omega_fetch_max_i32(int32_t *slot, int32_t value);
uint32_t omega_load_u32(const uint32_t *slot);
uint16_t omega_load_u16(const uint16_t *slot);
uint64_t omega_load_u64(const uint64_t *slot);
uint8_t omega_load_u8(const uint8_t *slot);
int32_t omega_load_i32(const int32_t *slot);

#define THREADS 8
#define ITERATIONS 20000

static uint32_t added;
static uint32_t exchanged;
static uint16_t narrow;
static uint64_t wide;
static uint8_t seen;
static int32_t highest;

static void *worker(void *argument) {
	long id = (long)argument;

	for (int i = 0; i < ITERATIONS; i++) {
		omega_fetch_add_u32(&added, 1);
		omega_cas_increment_u32(&exchanged);
		omega_fetch_add_u16(&narrow, 1);
		omega_fetch_add_u64(&wide, 3);
	}
	omega_fetch_or_u8(&seen, (uint8_t)(1u << id));
	omega_fetch_max_i32(&highest, (int32_t)id);
	return NULL;
}

int main(void) {
	pthread_t threads[THREADS];

	for (long i = 0; i < THREADS; i++) {
		if (pthread_create(&threads[i], NULL, worker, (void *)i) != 0) {
			fprintf(stderr, "pthread_create failed\n");
			return 1;
		}
	}
	for (int i = 0; i < THREADS; i++) {
		pthread_join(threads[i], NULL);
	}

	printf("stress-fetch-add-u32: %u\n", omega_load_u32(&added));
	printf("stress-cas-u32: %u\n", omega_load_u32(&exchanged));
	/* 8 * 20000 increments of a 16-bit counter wrap twice; the exact
	 * remainder is what proves no update was lost. */
	printf("stress-fetch-add-u16: %u\n", (unsigned)omega_load_u16(&narrow));
	printf("stress-fetch-add-u64: %llu\n", (unsigned long long)omega_load_u64(&wide));
	printf("stress-fetch-or-u8: %u\n", (unsigned)omega_load_u8(&seen));
	printf("stress-fetch-max-i32: %d\n", omega_load_i32(&highest));
	return 0;
}
