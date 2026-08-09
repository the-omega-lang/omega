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
can be a literal, `&[...]` compile-time slice, or a bare `[...]`
compile-time array literal, against a `[N]T`-typed header field — see
[strings, casting & slices](11-strings-casting-and-slices.md)). An explicit
tag (`tag: u32`) is optional — if omitted, the compiler assigns one; if
given, it must lead the header and each variant supplies a unique
compile-time value for it first.

Accessing a header or shared-dynamic field never requires knowing which
variant you're holding; accessing a body field does (see refinement,
below).

The tag and the header fields are **never writable** — they are per-variant
constants, and a write through any of them would desynchronize a live value's
tag from its actual layout. That is enforced at the one place every write
funnels through (`require_mutable_place`), so all five write forms are
rejected identically:

```
e.tag = 5;          # error: cannot assign to 'tag' of an enum value
e.tag += 1;         # same error
++e.tag;            # same error
p := &mut e.tag;    # same error
e.tag.some_mut_self_method();      # same error
```

(Fixed: the check used to live only in the plain-`=` path, so the other four
forms silently compiled.) Shared dynamic fields and a variant's own body
fields are ordinary runtime storage and stay freely assignable through all of
them. A bodyless variant with no shared dynamic fields either needs no
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
    .. => <*u8>b"more than a hundred\0",
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

**Exhaustiveness is real and enforced** (`compiler/omega-analyzer/src/
exhaustiveness.rs`): a sort-by-lo sweep over intervals detects every
overlap (hard error, no first-match-wins semantics like a plain `if`-chain
would have) and every gap (error unless `else` covers it, or exactly one
bare `..` catch-all arm does — see "Ranges" below). Scoped today to
**enums, integers, `bool`, and `char`** (see
[primitives](01-primitives.md) for `char`'s own domain) — a float
scrutinee is a clear `UnsupportedMatchScrutinee` diagnostic, not a silent
gap.

Because overlap is an error, a value `match`'s arms must **partition** the
domain: there is no trailing catch-all arm the way a plain `if`-chain's
final `else` is — a pattern that overlaps an earlier one is always an
error, never silently shadowed by first-match-wins. Use `else`, or a bare
`..` arm, for the "anything else" case. The overlap diagnostic reports the
pair in *source* order and says so explicitly:

```
error: overlapping match arms
  |
4 |         0..<10 => 1,
  |         ------ first covered here
5 |         5..<20 => 2,
  |         ^^^^^^ this pattern covers values an earlier arm already covers
  |
  = note: `match` has no first-match-wins rule -- every value must be
          covered by exactly one arm
```

(Fixed: the sweep finds an overlapping pair in *interval* order, which is not
the order the arms were written — a catch-all written last still sorts first,
so the old diagnostic blamed the arms *above* it and called them
"unreachable", which was also untrue for a merely partial overlap.)

`match` keeps its own `CheckedExpr::Match`/`emit_match` rather than fully
desugaring into `if`: an exhaustive match with no user `else` must *trap*
on the impossible remaining case rather than falling through with an empty
value, since a non-`void` match result feeding a control-flow merge point
needs every path to supply a value.

## Ranges

One grammar, shared verbatim by `match` range-patterns, slicing
(`base[range]`), and — legal only as a range-driven `for` loop's own
direct iterator source (see [`for`..`in` loops](18-for-in-loops.md)) — a
standalone range expression:

```
..=b     # [MIN, b], inclusive end
a..=b    # [a, b], inclusive end
..<b     # [MIN, b), exclusive end
a..<b    # [a, b), exclusive end
..       # fully open; both ends inferred from context
a..      # [a, inferred]
```

`..=`/`..<` both always require an explicit end (`a..=`/`a..<`, or bare
`..=`/`..<`, are all parse errors) — deliberately two different tokens
from each other and, on purpose, from `...` (still used, unrelated, for
variadic function parameters): writing `..=` forces a real choice between
inclusive and exclusive, rather than reaching for a habitual `..`-typo
that silently means the wrong thing.

`..`, by contrast, is the one spelling that's legal with **no** end at
all, ever — what it actually means is inferred from whichever position
consumes it: a slice's own container length (`&arr[5..]` — from index 5
to the end; bare `&arr[..]` — a full view), or a `match` arm's own
unmatched remainder (below). Its own start is independently optional from
its end in every shape above, the same as `..=`/`..<`.

Range bounds work for any type with an `integer_domain()` (see
[primitives](01-primitives.md)) — including `char`:

```
match c {
    'A'..='Z' => 1,
    'a'..='z' => 2,
    '0'..='9' => 3,
} else { 0 }
```

A `char` range is only ever meaningful as a `match` pattern — `char` has
no arithmetic, so unlike an integer range there's no sensible notion of
"step" or iteration over one (and, correspondingly, `char`/`bool` are
*not* legal range-driven `for`-loop element types even though both have
an `integer_domain()` — see [`for`..`in` loops](18-for-in-loops.md)).

### The `..` catch-all arm

A bare `..` arm (nothing written on either side) is legal in `match`, and
means "whatever's left uncovered by every other arm" — inferred, not
written, and only accepted when that remainder is unambiguous:

```
match some_integer {
    ..<0 => { /* negative */ }
    0 => { /* zero */ }
    .. => { /* from 1 to the domain's own max -- the one contiguous gap left */ }
}
```

This works for an enum `match` too, covering every variant no earlier arm
named:

```
match e {
    MyEnum::First => { ... }
    .. => { /* every other variant */ }
}
```

Unlike `else`, a `..` arm is subject to the same overlap-safety proof as
every other arm — it isn't an opaque fallback, it's a real, inferred
interval (or, for an enum, a real set of variants), so a later arm that
happened to also cover part of it would still be caught as
`OverlappingMatchArm`. At most one `..` arm is allowed per `match`
(`MultipleCatchAllPatterns` otherwise — there's only one "everything
else" to have), and it must actually have something left to cover
(`CatchAllPatternRedundant` if the other arms are already exhaustive on
their own). For a numeric/`bool`/`char` match specifically, what's left
must also reduce to exactly **one** contiguous range — deliberately not
stretched further than that:

```
match some_integer {
    0 => { ... }
    .. => { ... }   # error: CatchAllRangeNotInferable -- removing `{0}`
                     # from the domain leaves TWO disjoint ranges (below
                     # and above zero), not one
}
```

An enum `match`'s own `..` has no such contiguity requirement (variant
tags aren't required to be contiguous, and there's no "range" concept
for them at all) — any non-empty set of uncovered variants is fine.

## Caveats

- **`match` scrutinee unification is not part of literal-inference** — an
  arm-body's own type isn't coerced against a match's other arms the way
  `if`-branches are; this was deliberately excluded from the literal-
  inference feature (judged too entangled with exhaustiveness/refinement to
  fold in safely at the time).
- A float scrutinee is explicitly unsupported in `match`
  (`UnsupportedMatchScrutinee`), not silently mishandled — `char` is
  supported (see above).
