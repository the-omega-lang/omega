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
construction, and calls into other functions (same-module or
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
- **`sizeof<Type>`.**
- **Reading a non-`comp` global.** Only `comp`-bound identifiers (no
  storage, pure substitution) are readable from inside a `comp`
  evaluation; referencing an *earlier* `comp` binding works fine
  (ordinary substitution, same as calling an earlier-defined function
  already works).
- **An enum's header or dynamic fields**, either read or constructed via
  `comp` — only the tag and a variant's own body fields are supported
  today (see `ConstValue::Enum`'s doc comment). The header is a
  per-variant constant with no per-instance storage of its own, so this
  is a narrower gap than it might sound — see [enums & pattern
  matching](05-enums-and-pattern-matching.md).
- **Taking the address of, slicing, or calling a method on a `comp`
  binding** (`&SIZE`, `SIZE[0..1]`, `SIZE.method()`) — soundly supporting
  this means producing `ConstValue::Ref` the way `&<place>` *inside* a
  `comp` evaluation already does (see below), threaded through every
  place-producing call site, not just one; deliberately deferred rather
  than reaching codegen with a `Storage::Comp` place it has no defined
  meaning for. Plain field access (`SIZE.field`) is unaffected — only
  address-of/slice/method-call are rejected, whether the binding is local
  or top-level.

## `&<place>` inside a `comp` evaluation

Taking the address of an already-comp-evaluated place (`&x` where `x` is
itself being computed inside the same `comp` evaluation) produces
`ConstValue::Ref` — the address of another piece of `comp`-evaluated data,
generalizing what a compile-time string/slice literal already does
(both are secretly "pointer to a separately-built rodata blob") into one
explicit case. This is what will let a `comp`-constructed value contain a
pointer into another piece of `comp`-computed data (e.g. a fixed buffer),
not just string/slice literals — the interpreter and codegen both already
support it; only the "take the address of a whole `comp` *binding* from
outside a `comp` evaluation" case above is the deferred one.

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
