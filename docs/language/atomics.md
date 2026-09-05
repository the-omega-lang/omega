# Atomics

This chapter is normative for current Omega language behavior. Known implementation limitations are tracked separately under [`../issues/`](../issues/).

Atomic operations are declared as ordinary capability gaps in
[`core::atomic`](../guide/core-library.md) and implemented by the selected
platform; see [`gaps-and-glue.md`](gaps-and-glue.md). The compiler introduces
no atomic instruction, intrinsic, or target capability table of its own.

That division is about *who supplies* atomicity, not about what it means. The
rules below are the observable contract every implementation of the
`AtomicityN` gaps must satisfy, so portable code may rely on them without
knowing which mechanism a platform chose. They are not implementation-defined.

## Atomic locations

An **atomic location** is an address together with an access width. The four
width capabilities are `Atomicity8`, `Atomicity16`, `Atomicity32`, and
`Atomicity64`, operating on 1, 2, 4, and 8 bytes respectively.

Every operation of a width gap is **indivisible** with respect to every other
operation performed through that same gap on the same address: a concurrent
observer of that location sees a state either entirely before or entirely
after the operation, never a partial one.

The guarantee covers exactly that. It does **not** cover:

- concurrent atomic operations whose byte ranges overlap without sharing the
  same starting address and width;
- concurrent ordinary (non-atomic) reads or writes of bytes an atomic
  operation is accessing;
- byte ranges reached through two different width gaps at once.

Programs must not perform those accesses concurrently. Sequential
(non-concurrent) ordinary access to the same storage is unaffected: an atomic
location is ordinary Omega storage, and the operations are ordinary calls.

An implementation must accept every address that is valid for ordinary Omega
access at that width. Alignment beyond what ordinary access already requires
is a platform's problem to emulate, never a restriction the portable API
imposes.

## Modification order

Each atomic location has a single total order over the values written to it,
its **modification order**. All execution contexts agree on it.

A load of a location reads some value from that order. Once an execution
context has observed a value, no later operation of that context on that
location may observe an earlier value in the modification order.

Every read-modify-write operation (`exchange`, every `fetch_*`, and a
successful `compare_exchange`/`compare_exchange_weak`) reads the value
immediately preceding its own write in the modification order. Two
read-modify-writes therefore cannot both read the same value and both write.

## Orderings

Orderings state the **minimum** synchronization an operation must provide. An
implementation may always provide more (see "Strengthening" below).

Each operation category has its own ordering type, so an ordering that has no
meaning for an operation is not expressible for it:

| Type | Variants | Used by |
|---|---|---|
| `AtomicLoadOrdering` | `Relaxed`, `Acquire`, `SeqConsistent` | `load`, and the failure path of compare-exchange |
| `AtomicStoreOrdering` | `Relaxed`, `Release`, `SeqConsistent` | `store` |
| `AtomicRmwOrdering` | `Relaxed`, `Acquire`, `Release`, `AcquireRelease`, `SeqConsistent` | `exchange`, `fetch_*`, and the success path of compare-exchange |

Passing an ordering of the wrong category is an ordinary type mismatch,
reported at compile time. No atomic operation validates an ordering at run
time or panics because of one.

The guarantees are:

- **`Relaxed`** guarantees indivisibility and the modification order of its own
  location, and nothing about the order of other locations.
- **`Acquire`** (loads and read-modify-writes): if this operation reads a value
  written by a `Release` (or stronger) operation on the same location, every
  memory effect that preceded that release in its own execution context
  **happens before** every effect following this acquire.
- **`Release`** (stores and read-modify-writes): every memory effect preceding
  this operation in its own execution context is published to whichever
  acquire operation later reads its written value from that location.
- **`AcquireRelease`** (read-modify-writes only): the read side is an acquire
  and the write side a release.
- **`SeqConsistent`**: the operation additionally participates in one global
  total order over all `SeqConsistent` operations in the program. That order is
  consistent with each execution context's program order and with every
  location's modification order, and it carries the acquire/release guarantees
  of its category.

`Relaxed` orders nothing beyond its own location, so it never establishes
happens-before between execution contexts on its own. Acquire/release pairs on
one location are the portable way to publish and then observe unrelated data
written before the release.

## Operations

For each width, a platform provides:

```omega
load(location: *u32, order: AtomicLoadOrdering) => u32;
store(location: *mut u32, value: u32, order: AtomicStoreOrdering) => void;
exchange(location: *mut u32, value: u32, order: AtomicRmwOrdering) => u32;
compare_exchange(location: *mut u32, expected: u32, desired: u32,
                 success: AtomicRmwOrdering, failure: AtomicLoadOrdering) => Result<u32, u32>;
compare_exchange_weak(...) => Result<u32, u32>;
fetch_add / fetch_sub / fetch_and / fetch_or / fetch_xor(location: *mut u32, value: u32, order: AtomicRmwOrdering) => u32;
fetch_min_unsigned / fetch_max_unsigned(location: *mut u32, value: u32, order: AtomicRmwOrdering) => u32;
fetch_min_signed / fetch_max_signed(location: *mut i32, value: i32, order: AtomicRmwOrdering) => i32;
```

Only `load` takes an immutable pointer. Every operation that can modify the
location takes `*mut`, so an atomic call is never a way to write through an
immutable binding: a caller that wants operation-level atomicity on plain
storage provides mutable storage for it.

`exchange` and every `fetch_*` return the value the location held immediately
before that modification. `fetch_min_*`/`fetch_max_*` are read-modify-writes
whether or not the comparison selects a new value; the unsigned and signed
members differ in how they compare, not in how they order.

Atomicity changes the arithmetic in no way: `fetch_add`/`fetch_sub` compute the
same result the corresponding non-atomic operator computes for that type, and
the bitwise members compute the corresponding bitwise operator's result.

## Compare-exchange

`compare_exchange` compares the location's current value with `expected`. On
success it writes `desired` and returns `Ok` carrying the previous value,
which necessarily equals `expected`. On failure it writes nothing and returns
`Err` carrying the value it actually observed.

`compare_exchange_weak` may additionally fail **spuriously**: it may return
`Err` even when the observed value equals `expected`. Its `Err` still carries
the value it observed. A platform may implement the weak form as the strong
one, so a program must not treat a spurious failure as something it can
observe reliably; the weak form belongs in a loop that retries.

Success uses the operation's `AtomicRmwOrdering`. Failure performs no write,
so it is a load and uses an `AtomicLoadOrdering`. The two paths are
independent: Omega does **not** require the failure ordering to be no stronger
than the success ordering. Each path requests its own minimum, and an
implementation whose primitive cannot express the pair exactly strengthens one
of them rather than rejecting the call.

## Strengthening

An implementation may always provide more ordering or serialization than
requested, because these guarantees are minimums. A `Relaxed` operation may be
sequentially consistent in practice; a platform may serialize unrelated
locations; a lock-backed implementation that makes every operation effectively
sequentially consistent satisfies every request. A program must not infer the
absence of ordering from having asked for less.

Consequently, no operation promises lock-freedom, wait-freedom, or a bounded
execution time. An atomic call may block, and on some platforms it may be a
call into an operating system.

An atomic operation must not allocate: no implementation of these gaps routes
through `core::platform::GlobalAllocator`. Static, pre-existing, or
operating-system-owned synchronization state is permitted; hidden Omega heap
allocation as part of an atomic call is not.

## Platform obligation

A platform that fills a width gap implements the complete set of operations
for that width. An ISA that lacks a direct instruction for some member is
still required to provide it -- through a CAS or LL/SC retry loop, a lock,
interrupt masking where that is semantically sufficient, or an operating
system service. A platform that cannot uphold the contract for a width does
not fill that width's gap at all; a partially honest implementation is not
permitted.

Because gap calls are ordinary calls, a platform must also expect that the
compiler treats them conservatively: today they act as compiler memory
barriers regardless of the requested ordering. That is a stronger observable
behavior than requested, which the strengthening rule permits.

## Wrappers

[`std::atomic`](../guide/standard-library.md) provides `AtomicU8`–`AtomicU64`,
`AtomicI8`–`AtomicI64`, and `AtomicBool` over these gaps. The wrappers own
storage and naming only: they add no atomicity, no ordering, and no
lock-freedom to what the platform supplies, and their methods have the
semantics defined above.
