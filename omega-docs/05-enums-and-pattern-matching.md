# Enums & pattern matching

Omega enums merge two ideas: C/Java-style constant-bearing variants and
Rust-style payload variants, in one type. Layout is always
`[tag][header fields][shared dynamic fields][payload]`.

## Field categories

```
enum MyEnumWithExplicitTagAndHeader(tag: u32, exposed description: *str) {
    First(10, "This is the first entry"),
    Second(20, "This is the second entry"),
}

enum MyEnumWithSharedDynamicFields(exposed description: *str) {
    exposed favorite_number: i32;     # shared dynamic field
    exposed favorite_color: *u8;

    First("first"),
    Second("second") { exposed specific_field: i32; }   # body field, this variant only
}
```

Three orthogonal categories, each with a different combination of
"constant vs. runtime" and "every variant vs. one variant":

| Category | Where declared | Value | Present on |
|---|---|---|---|
| **header** (`Enum(...)`) | parenthesized after the enum name | compile-time constant, fixed per variant | every variant |
| **shared dynamic** | right after `{`, before the first variant | ordinary runtime value, freely assignable | every variant |
| **body** | inside one variant's own `{ ... }` | ordinary runtime value | that variant only |

Header values are literally compile-time constants baked per-variant (they
can be a literal, `&[...]` compile-time slice, or a bare `[...; N]`
compile-time array — see
[strings, casting & slices](11-strings-casting-and-slices.md)). An explicit
tag (`tag: u32`) is optional — if omitted, the compiler assigns one; if
given, it must lead the header and each variant supplies a unique
compile-time value for it first.

Accessing a header or shared-dynamic field never requires knowing which
variant you're holding; accessing a body field does (see refinement,
below). A bodyless variant with no shared dynamic fields either needs no
braces at all (`First(10, "...")`); once the enum has *any* shared dynamic
field, every variant needs a body listing at least those.

## Construction

```
t := MyEnumWithBodies::Third { some_number = 777u16; pointer_to_another_instance = &s; };
```

Same `name = value;` struct-literal grammar reused verbatim for a variant's
body (dynamic fields first, then that variant's own body fields) — no
separate construction syntax was invented for shared dynamic fields.

## Refinement, narrowing, and sum-type subtyping

`ResolvedType::Enum { cell, variant: Option<usize> }` — a value can be
*statically refined* to a specific variant, and Omega tracks this two ways:

- **Permanent refinement**: `s := MyEnumWithBodies::Second { ... };`
  infers `s`'s type as `MyEnumWithBodies::Second`, not the plain enum type
  — `&s` stays refined (`accepts` forbids ever assigning a *different*
  variant into a permanently-refined binding, so the pointee can't change
  shape underneath a live pointer), letting it flow directly anywhere a
  `*MyEnumWithBodies::Second`-specific pointer is expected. This is real
  sum-type subtyping through proofs, not just a convenience: no `match` is
  needed once the proof already happened at construction.
- **Match narrowing**: proving a variant via `match` re-declares a
  bare-identifier scrutinee in a fresh inner scope with the refined type —
  ordinary scope-shadowing, not a separate mechanism. Only a bare
  identifier narrows; a field access, deref, or computed expression still
  matches/branches correctly but isn't narrowed (and is evaluated exactly
  once into a synthesized local, so side effects don't re-run per arm).

`&` (immutable) preserves refinement for a *permanent* binding but erases
it for a match-arm shadow (whose refinement is only true for that lexical
scope). `&mut`/`mut self` **always** fully widen, both the resulting
pointer and the source binding's own tracked type from that point on — see
[variables & mutability](02-variables-and-mutability.md) for the aliasing
gap this closes and its own remaining caveat.

## `match`

```
message := match matched_num {
    0..<100 => <*u8>b"less than a hundred\0",
    100 => <*u8>b"a hundred\0",
    101... => <*u8>b"more than a hundred\0",
};

kind := match s {
    MyEnumWithBodies::First => <*u8>b"first\0",
    MyEnumWithBodies::Second => <*u8>b"second\0",
    MyEnumWithBodies::Third => <*u8>b"third\0",
};
```

`match scrutinee { pattern => body, ... } else { ... }` — arms are
comma-separated (optional trailing comma), uniformly whether `body` is a
bare expression or a `{ ... }` block. A pattern is either an `Enum::Variant`
path (optionally binding fields), a literal/range value, or a range (see
below). `else` is only required when the compiler can't already prove
exhaustive coverage.

**Exhaustiveness is real and enforced** (`omega-analyzer/src/
exhaustiveness.rs`): a sort-by-lo sweep over intervals detects every
overlap (hard error, no first-match-wins semantics like a plain `if`-chain
would have) and every gap (error unless `else` covers it). Scoped today to
**enums, integers, `bool`, and `char`** (see
[primitives](01-primitives.md) for `char`'s own domain) — a float
scrutinee is a clear `UnsupportedMatchScrutinee` diagnostic, not a silent
gap.

`match` keeps its own `CheckedExpr::Match`/`emit_match` rather than fully
desugaring into `if`: an exhaustive match with no user `else` must *trap*
on the impossible remaining case rather than falling through with an empty
value, since a non-`void` match result feeding a control-flow merge point
needs every path to supply a value.

## Ranges

One grammar, shared verbatim by `match` range-patterns and slicing
(`base[range]`):

```
...      # fully open
a...     # [a, MAX]
...b     # [MIN, b]
a...b    # [a, b], inclusive
a..<b    # [a, b), exclusive end
..<b     # [MIN, b)
```

There is **no plain two-dot `..`** anywhere in the language — every range
is unambiguously `...` (inclusive) or `..<` (exclusive-end). `..<` always
requires an explicit end (`a..<` and bare `..<` alone are parse errors) —
an open-ended exclusive range has nothing to exclude.

Range bounds work for any type with an `integer_domain()` (see
[primitives](01-primitives.md)) — including `char`:

```
match c {
    'A'...'Z' => 1,
    'a'...'z' => 2,
    '0'...'9' => 3,
} else { 0 }
```

A `char` range is only ever meaningful as a `match` pattern — `char` has
no arithmetic, so unlike an integer range there's no sensible notion of
"step" or iteration over one.

## Caveats

- **Generic enums with methods are fundamentally broken** — even a method
  with no generics or matching involved at all fails signature collection
  with a confusing `'MyOpt' expects 1 type argument(s), found 0` pointed at
  the enum's own declaration. A non-generic enum with an identical method
  works fine. This is why `omega-core` has no `Option<T>`/`Result<T>` —
  see [generics](06-generics.md) and [core library](13-core-library.md) for
  the `(bool, out: *mut T)` pattern used instead.
- **`match` scrutinee unification is not part of literal-inference** — an
  arm-body's own type isn't coerced against a match's other arms the way
  `if`-branches are; this was deliberately excluded from the literal-
  inference feature (judged too entangled with exhaustiveness/refinement to
  fold in safely at the time).
- A float scrutinee is explicitly unsupported in `match`
  (`UnsupportedMatchScrutinee`), not silently mishandled — `char` is
  supported (see above).
