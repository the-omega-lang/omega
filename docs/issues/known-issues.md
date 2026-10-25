# Known issues

Concrete current compiler/library bugs and unsupported cases. Resolved issues are removed from this file; history remains in git and `docs/plan/`. Language semantics are normative in `docs/language/`, so an implementation deviation recorded here does not redefine the language.

## Codegen

- **`avr-none` object emission can fail at `-O0`.** See
  [compiler-limitations.md](compiler-limitations.md#avr-object-emission-needs-an-optimized-build).

- **A range-driven `for` loop no longer compiles to a bare three-clause
  loop, and will not until MIR-level optimization exists.** `for i in 0..<n`
  used to be intercepted by a dedicated analyzer desugaring that emitted a
  counter, a comparison and an increment directly. It is now an ordinary
  `ToIterator`/`Iterator` call chain over `core::range::Range<T>` and
  `RangeIterator<T>`, which is what makes ranges tangible values and removes
  every range special case from the compiler — but it means the emitted code
  is a `next()` call returning `Option<T>`, plus a match, per iteration.
  Recovering the old shape needs two MIR passes that do not exist yet:
  inlining, and scalar replacement of aggregates to dissolve the cursor
  struct into registers. LLVM's own optimizer does not do this for us
  without that MIR-level work first — it collapses the equivalent
  hand-written Rust because that code never had a cursor struct routed
  through an `Option<T>` match in the first place. This is a deliberate,
  accepted trade of generated-code quality for uniformity and a much
  smaller compiler; it is the single strongest motivating case for
  starting the MIR optimizer.

- **A generic direct `foreign` function is rejected rather than
  monomorphized.** `foreign(cc) name<T>(...) => T;`/`{ ... }` parses and
  carries its generics through HIR, but `collect_foreign_function_signature`
  rejects any non-empty generics list outright
  (`AnalysisErrorKind::GenericForeignFunctionUnsupported`) instead of
  instantiating a signature/mangled symbol per type argument the way an
  ordinary generic function does. Wiring real instantiation through
  `ItemKey`/the driver's generic-instantiation cache for foreign items was
  deliberately deferred rather than half-implemented.
  [foreign-function-interface.md](../language/foreign-function-interface.md)

## Types

- **`*str` is not actually guaranteed valid UTF-8.** See
  [language-limitations.md](language-limitations.md#strings-casting--slices).

- **`char`'s classifiers are ASCII-only, not Unicode.** `is_alphabetic`,
  `is_whitespace` and `to_ascii_*` cover the ASCII range and nothing beyond
  it; a `char` above `0x7F` is reported as neither alphabetic nor whitespace
  regardless of what Unicode says. Full classification needs property tables,
  which do not belong in a freestanding `core` without a decision about where
  that data lives and what it costs in code size. The names are deliberately
  honest about the `to_ascii_*` half; the `is_*` half is the one that could
  mislead. [primitives.md](../language/types-and-primitives.md)

- **`char`'s validity is a supported path, not an enforced invariant.**
  `char::from_u32` rejects out-of-range values and UTF-16 surrogates, and the
  direct `<char>some_u32` cast stays refused — but a pointer reinterpretation
  (`*<*char>&some_u32`) still produces an arbitrary bit pattern as a `char`,
  from any package. This is accepted rather than fixed: closing it means
  restricting pointer casts, which contradicts the honest-address model. It
  is recorded because several comments would otherwise be tempted to claim a
  `char` is always valid — the true statement is that the supported path
  always produces a valid one. [primitives.md](../language/types-and-primitives.md)

- **Only enum tags and `never` calls are checked at runtime; `bool` and `char`
  validity are not.** The compiler reports an invalid enum tag reaching a
  `match`, an anonymous-enum widening or `?`, and a `never`-declared call that
  returns (see
  [enums-and-pattern-matching.md](../language/enums-and-pattern-matching.md) and
  [types-and-primitives.md](../language/types-and-primitives.md)). Nothing
  equivalent exists for a `bool` holding a byte other than `0`/`1` or a `char`
  holding a non-scalar value: a `match` on one is an integer match over its
  domain, and the bit patterns outside that domain reach no check. This is
  deliberately deferred rather than fixed, and it is recorded so the enum-tag
  checks are not read as a general validity guarantee.

- **`std::fmt`'s float output is fixed-precision, not round-trip** — six
  fractional digits, with a scientific fallback below `1e-6` and at or above
  `1e19` whose normalization loop (repeated multiply/divide by ten) is itself
  lossy. `nan`/`inf`/`-inf` are exact. A shortest-round-trip formatter
  (Ryu/Grisu-class) is deliberate future work, not a narrow fix here.
  [console-io.md](../guide/console-io.md)

## Conformance and specs (`meet` / `primitive`)

Remaining known conformance/spec issues:

- **Blanket conform bodies are checked lazily**, and a blanket emits a body
  for every type it is *materialized* against, not every type that calls it.
  Goal-directed proving has reduced the materialization set — a type is only
  ever swept for the specs something actually asked about, so an unrelated
  blanket is no longer instantiated just because the type was queried for
  some other spec — but the real fix is demand-driven conformance emission
  rather than registration-driven; that is a change to how
  `check_conformance_bodies` is scheduled, not a local tweak. Not a
  correctness or binary-size problem: codegen puts each function in its own
  section and every link uses `--gc-sections`, so dead copies never reach
  the executable. [specs.md](../language/specs-and-conformance.md)


- **Latent blanket overlap is diagnosed at use, not declaration.** The
  compiler intentionally does not try to prove whether arbitrary spec bounds
  overlap. Two unrelated blankets become an `AmbiguousConformance` only when
  a concrete type satisfies both; this avoids rejecting declarations that
  can never apply together, at the cost of a downstream diagnostic.
  [specs.md](../language/specs-and-conformance.md)

## Gaps and glue

- **No default-bodied `gap` function** — every gap function must
  currently be a bare requirement; a body is rejected outright
  ([gaps-and-glue.md](../language/gaps-and-glue.md)).

- **No "override" or test-only glue concept** — a second `glue` for the
  same gap is always a hard error project-wide, with no way to shadow one
  intentionally. [gaps-and-glue.md](../language/gaps-and-glue.md)

- **An unfilled gap warns on every compilation, reachable or not** — the
  driver's gap sweep (`omega-driver/src/compile/signatures.rs::sweep_gaps`)
  emits `unfilled_gap` once per gap that no `glue` in the compilation
  implements, whether or not anything calls it, and the warning is
  deliberately non-suppressible. Every shipped platform glues every `core`
  gap it can honestly provide, so this is quiet for an ordinary build; it
  surfaces for a package compiled without a platform, or against a platform
  that leaves a gap unfilled on purpose (`avr-none`'s allocator and console).
  The warning's own help already concedes that an unglued, uncalled gap links
  fine; restricting it to gaps something actually calls, or letting a gap
  declaration opt out, needs a compiler change.
  [gaps-and-glue.md](../language/gaps-and-glue.md)

## Macros

- **Macro visibility is not transitive through imports.** A module's macro
  environment is built from its *own* import statements and each target's
  *own* definitions; an imported module's imports are never followed. This is
  what keeps the pre-pass acyclic. It means a package cannot curate a macro
  surface by chaining plain imports the way it can't curate an item surface
  that way either — the deliberate mechanism for that, for macros exactly as
  for items, is a macro `alias` (see [`aliases.md`](../language/aliases.md)).
  [macros.md](../language/macros.md), [visibility.md](../language/visibility.md)

- **A macro body can only reach another module's macro through an `alias`,
  not an `import`.** The environment `omega-driver` registers for a module --
  the one a nested invocation inside that module's macro bodies resolves in --
  is built from the module's own definitions and its macro aliases. Its
  `import` statements bind names only for invocations written directly in that
  file, and the module may not have been expanded yet when another package
  expands one of its macros. `runtime/core/panic.omg` therefore aliases
  `core::builtins`' location macros instead of importing them.
  [macros.md](../language/macros.md)

## Compiler internals

- **Macro expansion traverses the AST by hand.** See
  [compiler-limitations.md](compiler-limitations.md#parsing-macro-expansion-and-the-hir).

- **Diagnostic order and IR declaration order vary between runs.**
  Compiling the same package twice can print its warnings in a different
  order (for example `runtime/core`'s unglued-gap and unused-import
  warnings), and the emitted IR can declare the same external functions
  in a different order (for example `runtime/std`'s `atomic.ll` for
  `aarch64-linux`). The set of diagnostics and of IR lines is the same, so
  builds link identically, but the output is not byte-for-byte
  reproducible.

## Design debt worth watching

- **The contextual-keyword set grows with every feature, with no promotion
  policy.** Twenty-three words are now position-dependent keywords
  (`parser::contextual`). Each one is a place where a lookahead can commit
  too early and silently stop the word being usable as a name — three had
  already done exactly that. The registry plus its generated test make the
  set visible and guarded, but there is no stated rule for when a word
  should graduate to a real reserved keyword instead.
  [parsing-and-hir.md](../architecture/parsing-and-hir.md)
- **Omega's calling convention is not the platform C ABI.** The largest
  piece of deliberate debt in the compiler, and it was *deliberately
  preserved unchanged* when the LLVM backend landed — mirrored rather than
  fixed, so separately compiled packages keep agreeing with each other. Two
  facts, both in `omega-codegen/src/abi.rs`:

  1. **Aggregates are flattened into their scalar leaves**, each leaf
     becoming its own parameter. C's SysV instead classifies a struct into
     eightbytes: `struct { i32 a; i32 b; }` is *one* register under C, and
     *two* parameters here.
  2. **The return rule is `leaves > 2` → `sret`**, justified in its own doc
     comment by "x86_64 SysV has exactly two integer return registers
     (rax/rdx)". That is an x86_64 fact, and it is now applied unchanged to
     **every** architecture — aarch64 (x0/x1, plus x8 as the indirect
     result register) and riscv (a0/a1) included.

  C's variadic default-argument promotion *is* implemented correctly
  (`abi::variadic_promotion`), so variadic C interop is unaffected.

  What this does and does not break: Omega-to-Omega calls are correct on
  every target, because every separately compiled `omgc` invocation reads
  the same `AbiSignature` and therefore agrees exactly. Only the **C
  boundary** is wrong, and only for aggregates passed or returned **by
  value**. Scalars and pointers — all Omega's C interop uses today — are
  correct.

  To keep it that way rather than waiting for someone to discover it,
  an unsupported composite/by-value shape under a **non-Omega convention**
  (`c`, `sysv64`, ...) is a **hard error**
  (`AnalysisErrorKind::UnsupportedConventionByValue`) pointing back at this
  entry. `foreign` linkage by itself is not the trigger: a bare `foreign`
  function uses `CallingConvention::Omega` and the ordinary `AbiSignature`,
  so it accepts Omega composites by value like any other Omega call.
  `analysis::abi` owns the classifier and is applied at foreign
  declarations/definitions, function-typed foreign bindings, and indirect
  calls through a non-Omega function type. One classifier to replace once a
  real per-target ABI classifier exists; until then it turns a silent
  miscompile into a compile error.

  Fixing it properly means per-target, per-convention ABI classification in
  `abi.rs` (eightbyte classification for SysV, AAPCS for aarch64, ...). The
  translation point is now settled as `foreign(cc)` (see
  [`foreign-function-interface.md`](../language/foreign-function-interface.md));
  what remains open is only the aggregate classifier itself, not where the
  boundary is spelled.

- **Nothing gates a 32-bit target end to end.** Phase A of the
  LLVM-backend work made every width-sensitive analyzer question read the
  real target width, and `riscv32-none`/`thumbv7em-none` objects do emit —
  but the coverage stops at object emission plus two analyzer-level
  assertions (`comp sizeof<usize> == 4`, and a `usize` literal above
  `u32::MAX` being rejected). Nothing links or *runs* a 32-bit image, so
  32-bit codegen is proved only by inspection. A residual hardcode in
  `ResolvedType::cast_class`'s pointer arm survived Phase A for exactly
  this reason and was found only by reading the emitted IR by hand.
  Closing it needs a 32-bit runner (qemu-user, or a freestanding image
  plus a linker script, which needs an entry convention Omega has not
  defined — `Os::None` stops at "emits a correct `.o`" on purpose).

- Every new `Expression`/`HirExpr`/`CheckedExpr` variant needs updates
  across up to five separate exhaustive matches spread over multiple
  crates (macro expansion, prelude re-exports, HIR lowering, defer-id
  collection, codegen emission) — the compiler catches every miss as a
  hard exhaustiveness error, so nothing is silently skipped, but budget
  for it when adding new expression forms.

## Diagnostics

- **The indexing and slicing notes swap `*[]T` and `*[?]T`.** The notes
  for an unindexable value, an unsliceable value, and a slice with no end
  call `*[]T` an unsized array and `*[?]T` a slice, the reverse of
  [types-and-primitives.md](../language/types-and-primitives.md): `*[]T`
  is the slice and `*[?]T` the unknown-size array pointer.

- **No error codes, and no machine-applicable suggestions.** `Diagnostic`
  carries a message, labels, and `note:`/`help:` footers — there is no
  `E0308`-style stable code to look up or search for, and no structured
  "replace this span with this text" a tool could apply. Both are additive
  later, but every error site is a place that would need revisiting, so the
  shape is cheaper to decide early than late.
  [parsing-and-hir.md](../architecture/parsing-and-hir.md)

- **Type-level capture remains possible in macro-generated declarations.**
  Generic parameters and `Self` intentionally ignore macro origin, because
  they are substitution-bound rather than lexical bindings. A generated type
  parameter can therefore still capture a same-named type from a substituted
  argument. There is no in-tree instance; partitioning these bindings would
  break the `Self` uses in the primitive conformance macros.
  [macros.md](../language/macros.md)
