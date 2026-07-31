# Compile-time evaluation (`comp`)

```
add(a: i32, b: i32) => i32 {
    a + b
}

comp SIZE := comp add(10, 20);   # SIZE carries no storage -- every use is
                                  # substituted with 30, the already-known
                                  # value, at compile time.
b := comp add(20, 30);            # b is an ordinary local; only its
                                    # *initializer* was compile-time
                                    # evaluated -- it's 50 from the start.
```

`comp` marks compile-time evaluation, deliberately **not** a function-level
color: `add` above is an entirely ordinary function, usable at runtime or
compile time from any call site, with no separate declaration space. Any
function can be *attempted* at compile time, on demand — there is no
`comp fn`.

`comp` appears in two independent positions:

- **On an expression** (`comp <expr>`) — evaluate `<expr>` now, via a real
  interpreter over its own already-type-checked body
  (`omega_analyzer::comp_eval`). A general prefix expression, usable
  anywhere an ordinary expression is, not just in a binding's initializer.
- **On a binding** (`comp ident := ...`) — `ident` carries no storage of
  its own; every reference to it is substituted with its already-known
  value at compile time. Always immutable — `mut comp ident := ...;` is a
  hard error (`AnalysisErrorKind::MutCompBinding`), since a value already
  substituted everywhere it's used could never observe a later mutation.

Without the binding-level `comp`, an initializer's `comp <expr>` still
evaluates at compile time, but the binding itself (`b` above) is an
ordinary, `mut`-if-declared-`mut` runtime place — its value just happens to
start out already computed.

## Top-level (global) `comp` bindings

`comp ident := comp expr;` also works at module scope, visible to every
other item exactly like any other top-level name:

```
add(a: i32, b: i32) => i32 { a + b }

comp SIZE := comp add(10, 20);   # module-level -- no storage, substituted
                                   # into every reference, in this module
                                   # or any function that imports it.

uses_it_elsewhere() => i32 { SIZE * 2 }
```

A top-level `:=` binding must be `comp` — `ident := value;` with no `comp`
reports a dedicated error (`AnalysisErrorKind::TopLevelWalrusNotComp`)
rather than being accepted or silently misinterpreted: a non-`comp`
top-level global would need a real runtime constructor/init-order story
(closer to a C++ static initializer than anything this feature builds),
which is a distinct, larger piece of work nobody has asked for. `mut comp`
is rejected the same way it is locally.

Evaluated *eagerly*, during the binding's own signature resolution — not
deferred to a later body-check phase — since `comp <expr>` interprets as
an inherent part of ordinary expression analysis regardless of which phase
touches it first. This is the same "signature resolution needs a body-
shaped answer" situation a `spec T`-return-type function's own inference
already has to handle; the driver's per-item body-check cache
(`ensure_item_body`) and its cycle guard (`body_in_progress`) are what
make it safe for a `comp` global to (directly, or through another
function) reference something that in turn calls back into it, without
either hanging or corrupting driver state.

## What the interpreter can evaluate

`comp <expr>`'s operand is analyzed completely ordinarily first — full
type-checking, generic/overload/cross-module resolution, exactly as if
`comp` weren't there — and only the resulting, already-checked tree is
interpreted. This means the interpreter needs no type-checking logic of
its own, and covers everything ordinary expression analysis does:
arithmetic, comparisons, `if`/`match`/`while`/`for`, struct/enum/union
construction (including a variant's header and shared dynamic fields, not
just its tag and body fields), casts (including a fat-to-thin pointer
cast, `<*u8>some_str_or_slice`), `sizeof<Type>`, `defer` (queued per call
frame and run in FILO order once that frame's function finishes, exactly
like at runtime — after the return value is already fixed, so a defer can
never influence it), and calls into other functions (same-module or
cross-module) — including a function whose own body itself uses `comp`.

```
struct Point {
    exposed x: i32;
    exposed y: i32;
}

make_point(x: i32, y: i32) => Point {
    Point { x = x; y = y; }
}

comp origin := comp make_point(0, 0);
p := comp make_point(3, 4);   # an ordinary local; comp-computed initial value
```

A call's own step count (loop iterations and call depth both count against
one shared budget) is bounded, so a runaway compile-time loop or infinite
recursion is a clean diagnostic, not a hang.

### What it can't (yet)

Each of these is a real, explicit gap — reported as a clean
`AnalysisErrorKind::CompEvalFailed` diagnostic naming exactly what
blocked it (with a call-site trace back to the outermost `comp`, when the
failure happened several calls deep), never a crash:

- **Calling an `extern` function.** No compile-time meaning to execute a
  foreign/OS call inside the interpreter.
- **Dynamic dispatch** (`spec *Self`, a coercion or a call through one) —
  no compile-time meaning without real vtable data.
- **Calling through a function-typed variable or field** (an indirect
  call) — only a call to a plain named function is supported today. This
  includes a function-typed *field* reached through a `comp` binding
  (`comp_binding.callable_field()`) — a plain *method* call on a `comp`
  binding is a different, already-supported case (see "Taking the address
  of ... or calling a method on a `comp` binding" below).
- **Reading a non-`comp` global.** Only `comp`-bound identifiers (no
  storage, pure substitution) are readable from inside a `comp`
  evaluation; referencing an *earlier* `comp` binding works fine
  (ordinary substitution, same as calling an earlier-defined function
  already works).

### Taking the address of, range-slicing, or calling a method on a `comp` binding

`&SIZE`, `SIZE[0..1]`, and `SIZE.method()` (from *outside* a `comp`
evaluation, on a `comp`-bound identifier) all work, via **const
promotion** — the same answer Rust gives for the identical problem.
Rust's `const` is inlined at every use site (no storage of its own), but
`&SOME_CONST` (or an implicit `&self` a method call needs) triggers
promotion: if the value has no interior mutability/`Drop` glue, rustc
materializes one anonymous `'static` read-only allocation for it on
demand and takes a real address into *that*, rather than trying to
address the substituted value itself.

Omega does the same thing, reusing machinery that already existed for a
different reason: `&<place>` *inside* a `comp` evaluation already produces
`ConstValue::Ref` (see below), and codegen already knows how to emit any
`ConstValue` into an anonymous, content-deduplicated rodata blob (the same
family of machinery `"..."`/`b"..."` literals and `&[...]` compile-time
slices use). Taking `&SIZE` from outside a `comp` evaluation, or calling a
`*self`-receiver method on it, or range-slicing it, all just wrap the
binding's already-known value in that same `ConstValue::Ref` on demand and
let the existing emission path materialize it — no new codegen, only a
new analysis-time call site for it.

Two call shapes never need promotion at all, since they never need an
address:

- **Plain field access and single-element indexing** (`SIZE.field`,
  `SIZE[i]`) — substituted directly against the already-known value, no
  address involved either way.
- **A `self`-receiver method call** (`SIZE.method()` where `method` takes
  `self` by value) — `SIZE`'s value is simply substituted in as an
  ordinary by-value argument, exactly like passing any other
  `comp`-computed value into any other function.

A `*mut self`/`&mut` in any of these shapes is still rejected (with the
same diagnostic an ordinary immutable binding's `&mut` gets) — the
promoted data is always read-only rodata, so there is no writable storage
to hand out a mutable pointer into. Dereferencing a `comp` binding
directly (`*SIZE`, distinct from a `*self`-receiver method call on one)
remains unsupported.

## `&<place>` inside a `comp` evaluation

Taking the address of an already-comp-evaluated place (`&x` where `x` is
itself being computed inside the same `comp` evaluation) produces
`ConstValue::Ref` — the address of another piece of `comp`-evaluated data,
generalizing what a compile-time string/slice literal already does
(both are secretly "pointer to a separately-built rodata blob") into one
explicit case. This is what lets a `comp`-constructed value contain a
pointer into another piece of `comp`-computed data (e.g. a fixed buffer),
not just string/slice literals, and it's the exact same mechanism the
"taking the address of ... a whole `comp` *binding* from outside a `comp`
evaluation" section above reuses for const promotion — the two differ only
in *when* the promotion fires (always, inside the interpreter, vs. on
demand, from ordinary analysis).

## Codegen

A `comp`-bound identifier never reaches codegen as a place at all — it's
substituted away during analysis into `CheckedExpr::Const(value)`, exactly
like an ordinary literal, so codegen's job is no larger than "emit a
constant" (already-proven machinery, now extended to structs/enums/unions/
refs, not just numbers/strings/slices). A non-`comp` binding with a `comp`
initializer is an ordinary place whose *initial* bytes happen to already
be known — struct fields are emitted as concatenated leaves (no byte-offset
math needed, mirroring an ordinary struct literal), while enum/union
values are built in an anonymous stack slot at their real byte offsets
(mirroring ordinary `EnumConstruct`/`UnionConstruct` codegen) and read
back out as leaves.

Every anonymous rodata blob a `ConstValue` (or a `&[...]` compile-time
slice) gets emitted into is content-addressed and deduplicated by
`Codegen::const_blobs`, keyed by the same content hash used to name the
blob's own symbol — needed once const promotion (above) could reasonably
emit *the same* comp value's content from two independent call sites
(e.g. `&SIZE` and a `*self`-receiver method call on `SIZE`, both in the
same compile): without a dedup check before `define_data`, the second
emission would try to define an already-defined symbol and the linker
step would reject it.

## Consolidation with existing constant positions

An enum variant's tag/header value and a `&[...]` compile-time slice
literal were always implicitly compile-time-only positions (no `comp`
keyword needed there, and still none is) — their own literal recognition
(a bare number/string/bool/char/array) is unchanged, but a shape that
isn't a recognized literal now falls back to the same general interpreter
`comp <expr>` uses, rather than being rejected outright. An enum header
value can therefore be any compile-time-evaluable expression, not just a
literal — no `comp` keyword needed even here, since the position is
already unambiguously compile-time-only:

```
compute_default_limit() => i32 { 10 + 5 }

enum Setting(exposed limit: i32) {
    Default(compute_default_limit()),   # no `comp` needed -- this position
                                          # already only ever means "evaluate
                                          # this at compile time"
}
```
