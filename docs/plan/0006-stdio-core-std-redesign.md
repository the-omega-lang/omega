# `core` and `std`: responsibilities, and a stdio redesign

## Task Description

- **What is being asked:** Make `core` actually be what it claims to be — the thin layer
  binding the language, the compiler, and the platform — by moving everything else to `std`,
  and redesign console I/O from scratch on the way.

  Three deliverables:
  1. **Enforce the split.** `cmp`, `default`, `hash`, `fmt`, `io` leave `core`, along with
     every `compose <primitive> : <spec>` block. `core` keeps the gaps, `Option`, the
     iteration protocol, and `primitive` blocks holding *inherent* methods only.
  2. **Redesign stdio.** `Writer`/`Reader` are replaced by a `Write`/`Read` spec pair,
     separate concrete sinks, and buffering as a generic adapter.
  3. **Make the split link-enforced.** Emit per-function sections so a target that uses no
     gap-touching feature links `core.o` with no glue at all.

- **Purpose:** `core` today violates its own stated contract. A program that calls only
  `i32::abs()` and `[?]u8::is_empty()` — no I/O whatsoever — **cannot link against
  `core.o`**, because `core::io::stdout_sink` carries a live relocation to
  `StandardOutput::write`. "Core is standalone, uniform and readily available on every
  platform, including embedded systems with no allocators, stdio, etc." is not true of the
  current tree, and it is not a documentation problem: it is a real link failure on exactly
  the target that matters most.

- **Reasoning:**

  **Why the split is now possible, and wasn't before.** `Hash` had to live in `core` because
  only `core` could write `spec Hash ... for i32` — `spec ... for` was confined to `core`, so
  any spec a primitive needed to satisfy was dragged in with it. `compose` removed that: any
  package owning a spec may compose it onto a primitive under the orphan rule. `std` can own
  `Hash` and write `compose i32 : Hash`. This was the blocker, and it is gone.

  **The migration line already exists in the source.** Removing the inherent-satisfies-a-
  requirement fallback forced every spec-satisfying body into its own `compose` block, so
  `core`'s primitive modules are already sorted:

  ```omega
  primitive $T {                       # stays in core: pure, no gaps, no Writer
      exposed clamp(...) exposed pow(...) exposed abs(...) exposed signum(...)
      exposed is_even(...) exposed is_odd(...) exposed is_negative(...) exposed is_positive(...)
  }
  compose $T : Ord     { equals compare min max }   # -> std
  compose $T : Default { default }                  # -> std
  compose $T : Hash    { hash }                     # -> std
  compose $T : Display { fmt }                      # -> std
  ```

  The plan moves whole blocks, not individual functions. That is why this is tractable at
  all, and it is worth doing now while the line is still sharp.

  **What is wrong with `Writer`.** Seven fields, six responsibilities:

  | concern | evidence |
  |---|---|
  | sink dispatch | `sink: (ctx: usize, data: *[?]u8) => usize` + `ctx: usize` |
  | buffering | `buf`, `cap`, `len`, `flush` |
  | sticky error latching | `failed`, `had_error()` |
  | memory-only mode | `memory_only` — a mode bit that *redefines what `flush` means* |
  | console identity | `stdout()`, `stderr()`, `stdout_buffered()`, `stderr_buffered()` |
  | text conveniences | `write_str`, `newline` |

  The `memory_only` flag is the clearest symptom: a boolean that changes a method's semantics
  is two types wearing one hat. `Reader` repeats the pattern and adds `read_line`, which puts
  line-splitting policy inside a byte source.

  The deepest problem is `(sink, ctx)`. That pair is a **hand-rolled fat pointer** — a code
  pointer beside a `usize`-punned data pointer — reimplementing dynamic dispatch by hand.
  Omega has real fat pointers with vtables (`spec *T`). It was written this way because there
  was no spec to dispatch on and no way to give a foreign type one. With `compose` there is,
  so `spec *mut Write` replaces the pair: type-safe, no punning, and `to_sink` disappears
  with it.

  **Why `Option<usize>` is the right gap signature.** `write(bytes, written: *mut usize) =>
  bool` splits one answer across a return value and an out-parameter, so a caller can observe
  `true` with `written` untouched, or ignore the `bool` and read garbage. `Option<usize>`
  makes the two states unrepresentable apart. `Option` lives in `core` and the gaps live in
  `core`, so there is no layering problem.

  Alternatives considered:
  - *Keep `fmt`/`io` in core and only move `cmp`/`hash`/`default`.* Rejected: it leaves the
    link failure in place, which is the concrete harm.
  - *Extend the ambient prelude to `std` so `println$` keeps working with no import.*
    Rejected (decided): pre-imported is what makes `core` special, and the reason `core` must
    stay thin. Extending it would make "pre-imported" mean "convenient" rather than
    "fundamental", and every name `std` later adds would silently enter every module.
  - *A generic `Display::fmt<W: Write>`.* Rejected: spec functions have no per-function
    generic bounds, so this is not expressible. `spec *mut Write` is, it is object-safe, and
    with a buffer underneath it costs one indirect call per *flush*, not per byte.
  - *Keep `Writer` and split only buffering out.* Rejected: `(sink, ctx)` and `memory_only`
    are the two worst parts and neither is buffering.

- **Resolved concerns:**
  - **`Eq`/`Ord`/`Ordering`/`Default`/`Hash` relocate to `std`, not deleted** (decided). They
    have no compiler integration so they have no claim on `core`, but `std`'s own definition
    covers them ("data structures" need hashing) and `HashMap`/`HashSet` keep working
    unchanged.
  - **The prelude stays `core`-only** (decided). 169 print-macro call sites across exactly two
    files (`examples/dev/main.omg`, `examples/io_demo/main.omg`) need one import line each;
    macros are importable through `Driver::macro_env` already.
  - **Per-function sections are in scope** (decided). Verified working:
    `ObjectBuilder::per_function_section(true)` takes `core.o` from 1 text section to 358, and
    with `--gc-sections` a no-I/O program links against `core.o` with **zero glue**. This is a
    one-line codegen change that turns the responsibility split from a convention into a
    link-time fact.
  - **`chars.omg` and `bools.omg` cease to exist.** Both contain only a `Display`
    conformance; once that moves there is nothing left. Their `primitive` blocks are empty.
  - **This is a breaking change to every consumer**, and deliberately so: `core::io::Writer`,
    `core::fmt::Display`, `core::cmp::*` all change package. `core` is pre-imported, so code
    that used them without an import now needs one.

## Technical Details

### The two packages afterwards

**`core`** — nothing here may reference a gap symbol.

| module | contents |
|---|---|
| `platform` | the four gaps, console signatures changed to `Option<usize>` |
| `option` | `Option<T>` — used by the gaps themselves |
| `iterator` | `Iterator<T>`/`ToIterator<T>`, the `for..in` protocol (compiler integration) |
| `numerics` | `primitive` blocks: `clamp`, `pow`, `abs`, `signum`, `is_even`, `is_odd`, `is_negative`, `is_positive`, float equivalents |
| `strings` | `primitive str`: `is_empty`, `starts_with`, `ends_with`, `contains`, `as_bytes` |
| `slices` | `primitive<T> [?]T`: `is_empty`, `get`, `first`, `last` |
| `core` | package root |

Deleted from `core`: `cmp`, `default`, `hash`, `fmt`, `io`, `chars`, `bools`.

**`std`** — gains `cmp`, `default`, `hash` (specs moved verbatim), every
`compose <primitive> : <spec>` block from `core`'s primitive modules, plus a rebuilt `io` and
`fmt`.

### The stdio design

Four layers, one job each.

**1. The byte contracts.** Both object-safe, so `spec *mut Write` exists.

```omega
exposed spec Write { write(*mut self, bytes: *[?]u8) => Option<usize>; }
exposed spec Read  { read(*mut self, into: *mut [?]u8) => Option<usize>; }
```

`None` is failure. A short write returns `Some(n)` with `n < bytes.length`; the caller
resumes. No sticky `failed` flag anywhere in the contract — latching, where wanted, belongs
to an adapter that documents it.

**2. Concrete sinks and sources, each its own type.** None knows about any other.

```omega
exposed marker Stdout {}   compose Stdout : Write { ... StandardOutput::write(bytes) }
exposed marker Stderr {}   compose Stderr : Write { ... StandardError::write(bytes)  }
exposed marker Stdin  {}   compose Stdin  : Read  { ... StandardInput::read(into)    }

exposed struct SliceWriter { ... }   # writes into caller storage; `None` when full
```

`memory_only` disappears: a full `SliceWriter` returns `None`, which is what `None` already
means. `Stdout`/`Stderr`/`Stdin` are markers — zero-sized, no state, and the only things in
the tree that name a console gap.

**3. Buffering as a generic adapter**, composing `Write` itself so it nests.

```omega
exposed struct BufWriter<W> { inner: *mut W; buf: *mut [?]u8; len: usize; }
compose<W> BufWriter<W> : Write { ... }
exposed struct BufReader<R> { ... }
```

Buffer storage stays caller-owned (no allocator dependency), exactly as today.

**4. Formatting, decoupled from every concrete writer.**

```omega
exposed spec Display { fmt(*self, out: spec *mut Write) => void; }
exposed write_uint(out: spec *mut Write, value: u64, base: u32) => void
exposed write_int / write_bool / write_char / write_float
```

`read_line` moves out of `Read` entirely — it is line-splitting policy, and belongs beside
the other `std::io` helpers as a free function over a `spec *mut Read`.

### What changes

**Compiler** (one line plus build recipes)
- `omega-codegen/src/cranelift/mod.rs` — `builder.per_function_section(true)` before
  `ObjectModule::new`.
- `justfile` — `--gc-sections` on every `cc` link line.

**`core`**
- `platform.omg` — three console gaps to `Option<usize>`; `import option::Option`.
- `numerics.omg`/`strings.omg`/`slices.omg` — delete every `compose` block and every import
  that only served one (`cmp::*`, `hash::Hash`, `fmt::*`, `io::Writer`).
- Delete `cmp.omg`, `default.omg`, `hash.omg`, `fmt.omg`, `io.omg`, `chars.omg`, `bools.omg`.

**`std`**
- New `cmp.omg`, `default.omg`, `hash.omg` — specs moved verbatim.
- New `primitives.omg` — every `compose <primitive> : <spec>` block relocated from `core`,
  including `chars`/`bools`' `Display`. Legal because `std` owns each spec.
- Rewritten `io.omg` and `fmt.omg` per the design above; the four print macros move here.
- `hash_map.omg`/`hash_set.omg` — import `Hash`/`Eq` from their new home; otherwise untouched.

**Examples and build**
- `examples/dev` gains a `std` dependency (`--extern=std:runtime/std/`, `build-exe` depends
  on `build-std`, `run-exec` links `target/std.o`) plus import lines for the print macros and
  any `Display`/`cmp` use.
- `examples/io_demo` — imports updated; its `Pair : Display` compose now names `std::fmt`.

### What must not change

- **`gap`/`glue` mechanism** — only the three console *signatures* change. `GlobalAllocator`
  is untouched, and one-glue-per-gap still holds.
- **`Option`'s variant order** (`None = 0`, `Some = 1`), load-bearing in `analyze_for_in`.
- **The composition model** — orphan rule, spec-namespace resolution, bound context. This
  plan is a client of `compose`, not a change to it.
- **`for..in`** and the `Iterator`/`ToIterator` protocol.
- **The prelude mechanism** — still `core`-only, unmodified.
- **`tests/io_demo.expected`** — the redesign must reproduce identical output.
- **Collections** — `List`, `LinkedList`, `HashMap`, `HashSet`, `String` keep their APIs.

### Chosen approach

Move first, redesign second. The relocation is mechanical (whole blocks, already separated)
and can be verified by the existing end-to-end tests before any I/O semantics change. Doing
the redesign first would mean rewriting `Writer` in `core` and then moving it, testing a
configuration that never ships.

Per-function sections land **first**, alone, because it is the acceptance test's instrument:
without it, "core links with no glue" cannot be observed even once it is true.

### Risks and open questions

- ~~Bounds on compose generic parameters~~ — **investigated, and worse than suspected. Now
  step 1**, a hard prerequisite for `BufWriter<W>`. See that step for the full finding.
- **`spec *mut Write` in a macro expansion.** The print macros construct a
  `BufWriter<Stdout>` and pass `&mut it` where `spec *mut Write` is expected. Coercion at a
  call argument is wired, but a macro-expanded call site is worth checking explicitly.
- **Per-function sections and debug info.** 358 sections where there was 1 may affect object
  size or any future debug-info work. Measure `ls -l target/core.o` before and after; a large
  regression is worth reporting even though it does not block.
- **`--gc-sections` is opt-in for consumers.** Anyone linking Omega objects by hand without
  it gets today's behaviour. That is acceptable (nothing breaks) but must be documented, not
  assumed.
- **`Display` through `spec *mut Write` costs one indirect call per `write`.** With buffering
  that is one per flush. If a hot path shows otherwise, say so rather than reintroducing a
  generic `Writer` parameter.

## Implementation Plan

1. **Compose generic bounds: check them, then seed them.** A hard prerequisite —
   `BufWriter<W>` in step 6 cannot be written without it, and it is a soundness fix in its
   own right.

   **The finding.** A compose's own generic bounds are parsed, stored, and then ignored
   entirely. Three reproducers, all verified:

   | shape | today |
   |---|---|
   | `compose<T: W> Buf<T> : W { ... self.inner.w(n) ... }` | rejected: "method 'w' comes from spec 'W' but is not in this bound context" |
   | `Buf<NotW>` instantiated against `compose<T: W>` | **compiles** — the bound is never checked |
   | that `Buf<NotW>` coerced to `spec *mut W` and dispatched | **compiles** — a vtable is built for a conformance that should not exist |
   | the same shape on an ordinary `struct Holder<T: W>` | correctly rejected |

   The last row is the point: ordinary generic *items* already check their bounds, in
   `Driver::check_generic_bounds` during `ensure_item`. `instantiate_compose` is the only
   instantiation path that does not, so a compose's bound is decorative — it neither
   constrains the instantiation nor informs the body. The failure that surfaces today
   (`W::w(self.inner, n)` with an unsatisfied `T`) is reported at the *call site inside the
   compose body*, blaming a line the author of `Buf<NotW>` never wrote.

   **The fix, both halves, in `instantiate_compose`** (`omega-driver/src/composes.rs`),
   immediately after the target resolves and before `check_compose_block` runs:

   - **Check.** For each of the compose's own generic parameters carrying a bound, resolve
     it under `substitution` and verify the concrete argument satisfies it, exactly as
     `check_generic_bounds` does. On failure, report `SpecNotImplemented` anchored at the
     *compose declaration* and return `None` — the entry must not be registered, so no
     conformance and no vtable can be derived from an unsatisfied bound.
   - **Seed.** Feed the satisfied bounds into the body's bound context, so
     `self.inner.write(...)` resolves. `Driver::bound_context_for` already produces exactly
     the right seed list, including transitive-dependency expansion, and
     `with_analyzer_in(module, generics, bounds, owner, f)` already takes a bounds slice —
     `instantiate_compose` currently calls `with_analyzer`, which hard-codes `&[]`. Both
     pieces exist; nothing new is needed.

   Reuse `check_generic_bounds`' machinery rather than reimplementing it. If it cannot be
   shared without contortion, extract the per-parameter check into a helper both call — do
   not fork the logic, or the two paths will drift on exactly the question of what a bound
   means.

2. **Per-function sections.** `per_function_section(true)`; `--gc-sections` in the `justfile`.
   Baseline `nm target/core.o`, section count, and object size before/after. All gates must
   still pass — this changes nothing observable except what the linker may drop.
3. **Gap signatures.** Three console gaps to `Option<usize>`; update `core::io`'s three bridge
   functions and `runtime/plat/libc/libc.omg`'s three glue blocks in place. Still in `core`,
   still building — this isolates the signature change from the move.
   **Relocation is atomic per spec.** `core` may never reference `std` — it does not
   register `std` as an extern and never can, since `std` depends on `core`. So for any spec
   `S` leaving `core`, every `core`-resident `compose <primitive> : S` must move *in the same
   step*: move the spec first and `core`'s conformances name a package `core` cannot see;
   move the conformances first and `std` composes a `core`-owned primitive with a `core`-owned
   spec, which `check_compose_orphan` rejects (a primitive's owner defaults to `core`). Both
   orders are impossible, so a spec and its primitive conformances are one indivisible move.
   Steps 4 and 5 are the two groups this rule produces; do not subdivide either. (This
   corrected an ordering contradiction in the first draft, which moved the `Display`
   conformances a step ahead of `Display` itself.)

4. **Relocate `cmp`/`default`/`hash` *with* their conformances.** The specs (`Eq`, `Ord`,
   `Ordering`, `Default`, `Hash`) to `std`, and in the same step every
   `compose <primitive> : {Eq,Ord,Default,Hash}` block out of `core`'s
   `numerics`/`strings` into `std::primitives`. Update `hash_map`/`hash_set` imports.
   `core`'s inherent primitive methods reference none of these — verified — so what remains
   in `core` still builds. `test-io` and `run-exec` must pass at the end of the step.

5. **Relocate `fmt` and `io` *with* the `Display` conformances.** `fmt` (the `Display` spec
   and the `write_*` helpers), `io` (`Writer`/`Reader`, the console bridges, the four
   macros), every `compose <primitive> : Display` block, and `std::io`'s existing helpers —
   all together, unchanged in behaviour. Delete `chars.omg` and `bools.omg`, which hold
   nothing but a `Display` conformance. Update `examples/dev` and `examples/io_demo` imports
   and the `justfile`'s `build-exe`/`run-exec`.

   `io` cannot be held back from this group: `Display::fmt` takes `*mut Writer`, and the
   print macros call `Display::fmt`, so leaving either behind puts a `core`→`std` reference
   in the macros. Holding `io` back would also leave the gap relocations in `core.o` and the
   acceptance test unreachable.

   **Acceptance test:** a package registering only `core`, using a `primitive` method and no
   I/O, links against `core.o` alone with no glue object. `nm -u target/core.o` shows gap
   *declarations* only, zero relocations.

6. **Redesign `std::io`.** `Write`/`Read`; `Stdout`/`Stderr`/`Stdin` markers; `SliceWriter`;
   `BufWriter<W>`/`BufReader<R>`; `read_line` as a free function. Delete `Writer`/`Reader`.
7. **Redesign `std::fmt`.** `Display::fmt` over `spec *mut Write`; `write_*` helpers
   likewise; rewrite the four macros onto `BufWriter<Stdout>`/`BufWriter<Stderr>`.
   `tests/io_demo.expected` must be byte-identical.
8. **Docs.** `13-core-library.md` and `23-standard-library.md` are rewrites, not edits.
   `24-console-io.md` describes a design that no longer exists. Also `08-specs.md`
   (primitive conformances move package), `10-modules-and-linkage.md` (per-function sections
   and `--gc-sections`), `21`/`22` (gap signatures), `14-known-issues.md`.

## Testing

**New cases — step 1 (compose generic bounds):**
- `compose<T: W> Buf<T> : W` whose body calls `self.inner.w(...)` through the bound, at two
  different `T`. This is what `BufWriter<W>` needs and it is rejected today.
- The same body calling `W::w(self.inner, ...)` spec-qualified — works today, must keep
  working, and must not be the *only* thing that works.
- `Buf<NotW>` against `compose<T: W>` → `SpecNotImplemented`, anchored at the **compose
  declaration**, not at a line inside its body. Compiles today.
- That same `Buf<NotW>` coerced to `spec *mut W` → rejected, no vtable emitted. Compiles
  today, which is the soundness half.
- A bound naming a spec *alias* (`compose<T: AB> Buf<T> : W`) resolves through its members,
  matching how an ordinary generic item's bound already behaves.
- An unbounded compose parameter (`compose<T> Box<T> : Show`) still works and gains no
  context — the seed must be exactly the declared bounds, never every compose on `T`.

**New cases — the split and stdio:**
- A no-allocator, no-stdio package: registers `core` only, calls `abs()`/`clamp()`/
  `is_empty()`, links `core.o` with **no glue object**. This is the plan's headline test and
  it fails today.
- `BufWriter<Stdout>` and `BufWriter<SliceWriter>` — the same adapter over two sinks.
- `BufWriter<BufWriter<Stdout>>` — nesting, proving the adapter composes `Write` honestly.
- A `SliceWriter` filled past capacity returns `None` rather than latching a flag.
- A short write from a sink resumes correctly (write returns `Some(n)`, `n < len`).
- `Display::fmt` through `spec *mut Write` for `i32`, `*str`, `bool`, `char`, `f64`.
- A user type composing `Display` in a third package, printed through `println$`.
- `read_line` over `spec *mut Read` against `Stdin` and against a slice source.

**Negative cases:**
- `compose Stdout : Write` in a package owning neither → `ComposeOrphanViolation`.
- A gap glue returning the old `bool`/out-pointer shape → `GlueFunctionSignatureMismatch`.
- Using `println$` without importing it → unresolved macro, naming the import.
- Referencing `core::fmt::Display` after the move → unresolved, ideally suggesting `std::fmt`.

**Regression risk:**
- `tests/io_demo.expected` byte-identical at the end of every step from 4 onward — it is the
  only check on real program output, and steps 4-7 relocate then rewrite the entire I/O
  stack underneath it.
- `just run-exec` exit 69 at every step; `examples/dev` exercises 153 print sites.
- `nm target/plat.o` — the glue symbols change at step 3 (signature change) and must not move
  at any other step.
- Steps 4 and 5 are each internally atomic (see the rule above the step list); neither may
  be split into a spec move and a conformance move, in either order.
- `compiler/omega-driver/tests/compose.rs` — several tests reference `core`-owned specs and
  will need their imports updated.

**Target coverage:**
- *Hosted:* `build-core`, `build-plat`, `build-std`, `test-io`, `run-exec`, `build-io-demo`.
- *No-allocator, no-stdio:* the headline test above — the configuration this whole plan
  exists to make possible.
- *Allocator only:* registers `core` + `plat`'s allocator glue but no console glue, uses
  `List`/`String`, links clean. Proves `std`'s "pay only for the glues you need" claim now
  holds at link time.
