# Enums and pattern matching

This chapter is normative for current Omega language behavior. Known implementation limitations are tracked separately under [`../issues/`](../issues/).

Omega enums are tagged sum types that can also attach per-variant constants and common runtime fields. Conceptually, an enum value contains a tag, any header fields, any shared dynamic fields, and storage for the selected variant's body fields. The exact ABI representation is documented separately in [`../architecture/abi-and-representation.md`](../architecture/abi-and-representation.md).

## Declaration

A basic enum is:

```omega
enum Ordering {
    Less,
    Equal,
    Greater;
}
```

When no explicit tag field is declared, every enum has an implicit `tag: u16`. Variants receive tag values in declaration order beginning at `0`.

An enum may declare a header:

```omega
enum Status(tag: u8, exposed label: *str) {
    Ok(0, "ok"),
    Err(1, "error") {
        exposed code: i32;
    };
}
```

If an explicit tag is present, it must be the first header field, must be named `tag`, and must have an integer type. Every variant supplies a unique compile-time-constant value for it. The remaining header fields are also supplied once per variant as compile-time constants.

An enum may additionally declare **shared dynamic fields** before its variants:

```omega
enum Message(exposed description: *str) {
    exposed timestamp: u64;

    Info("info"),
    Error("error") {
        exposed code: i32;
    };
}
```

Methods and static functions may appear after the semicolon terminating the variant list.

## Field categories

| Category | Declared at | Value kind | Present on |
|---|---|---|---|
| tag | implicit, or first header field | compile-time constant per variant | every variant |
| header field | `Enum(...)` | compile-time constant per variant | every variant |
| shared dynamic field | enum body before variants | runtime value per instance | every variant |
| variant body field | inside a variant body | runtime value per instance | one variant |

Header values must be compile-time representable for their declared type. This includes ordinary compile-time scalar values and the compile-time aggregate forms described in [`strings-casts-arrays-and-slices.md`](strings-casts-arrays-and-slices.md).

The tag and header fields are immutable. They may be read, but they may not be assigned through any mutation form. Shared dynamic fields and variant body fields obey the normal mutability rules from [`bindings-and-mutability.md`](bindings-and-mutability.md).

## Variant construction and refined types

A variant with no shared dynamic fields and no body fields is constructed by its path alone:

```omega
x := Ordering::Less;
```

A variant that has shared dynamic fields and/or body fields is constructed with a body literal:

```omega
err := Status::Err {
    code = 7;
};
```

Every runtime field required by that variant must be supplied exactly once: all shared dynamic fields plus all fields in the selected variant body. Unknown or duplicate fields are rejected. Header values and the tag are not supplied at construction; they are fixed by the variant declaration.

Construction produces a **refined variant type**. For example, the inferred type of:

```omega
s := MyEnum::Second { value = 10; };
```

is the refined type `MyEnum::Second`, not merely `MyEnum`. Code holding that refined type may access fields that exist only on `Second`.

A refined variant value can widen to its parent enum where required. Once only the parent enum type is known, variant-specific body fields are unavailable until the variant is proved again, normally by `match`.

Generic enum arguments can be explicit or, where enough type information exists, inferred under the generic rules in [`generics.md`](generics.md):

```omega
some := Option::Some { value = 5i32; };  # infers Option<i32>::Some
none: Option<i32> = Option::None;        # context supplies T
```

## Anonymous enums

An **anonymous enum** is a structural tagged sum type written directly in type
position:

```omega
enum ParseError | IoError
enum i32 | *str | Empty
enum OnlyOne
```

The variants of an anonymous enum are the member types themselves. There is no
declaration, no name, no header, no shared dynamic fields, and no method block:
an anonymous enum exists only as a type, so that a function can transport one of
several existing types without a nominal wrapper enum.

An anonymous enum is legal wherever an ordinary value type is legal: parameters,
return types, locals, globals, aggregate fields, generic arguments, alias
targets, `sizeof`, and compile-time values.

### Members

A member is written with ordinary type syntax, and `|` separates members. A
single-member anonymous enum (`enum A`) is legal.

A member must be a value type under the same rules that apply to an aggregate
field: a `spec` declaration and `never` are rejected, as are the bare `[]T` and
`[?]T` array forms.

An anonymous enum is a set of **leaf** member types. A member that is itself an
anonymous enum is **flattened**: its own members become members of the outer
enum, recursively. There is no parenthesized type syntax, so a nested anonymous
enum is written either through an alias or as the last member, where the inner
`enum` consumes the rest of the list:

```omega
alias Inner = enum A | B;
alias Outer = enum C | Inner;   # three members: A, B, and C
                                # the same type as `enum C | enum A | B`
                                # and as `enum A | B | C`
```

Flattening erases only an *immediate* anonymous-enum member. Every other type
constructor is a boundary:

- a named enum is nominal, so `enum Named | C` has exactly the two members
  `Named` and `C`, whatever variants `Named` declares;
- a type that merely *contains* an anonymous enum keeps it inside: with
  `alias Inner = enum A | B;`, `enum *Inner | C` has the members `*Inner` and
  `C`, not `*A | *B | C`. The same holds for arrays, function types, and
  generic arguments;
- an alias is not a boundary, and neither is generic substitution: when a
  generic argument lands an anonymous enum in immediate member position it
  flattens there too, and any duplicates that substitution introduces
  disappear before tags and layout are assigned.

Flattening never removes the enum itself: a single-member anonymous enum is
still an anonymous enum, so `enum A` is not `A`.

### Structural identity

An anonymous enum has no declaration, so its identity is entirely structural.
After every member is resolved to its final type, immediate anonymous-enum
members are flattened away, the remaining leaf members are deterministically
ordered, and exact duplicates are removed. The resulting **canonical member
list** is the type's identity.

Consequently:

- `enum A | B` and `enum B | A` are the same type;
- `enum A | A` is the same type as `enum A`;
- `enum C | enum A | B` is the same type as `enum A | B | C`;
- an alias adds nothing: given `alias Errors = enum ParseError | IoError;`,
  `Errors` and `enum IoError | ParseError` are one type with one layout and one
  symbol, exactly as [`aliases.md`](aliases.md) requires;
- two separately compiled packages that spell the same member set in different
  orders agree on layout, tag values, and mangled symbols.

Canonical ordering is a property of the semantic types involved, not of source
text, declaration order, or compilation order.

### Representation and tags

An anonymous enum uses the ordinary enum representation model described in
[`../architecture/abi-and-representation.md`](../architecture/abi-and-representation.md):
a `u16` tag followed by storage for the selected member's value. It has no
header fields and no shared dynamic fields, and each variant has exactly one
body field — the member value itself.

A member's tag is its index in the canonical member list, starting at `0`.
Because the canonical list does not depend on how the type was spelled, every
spelling of the same anonymous enum produces the same tag for the same member.

An anonymous enum may therefore have at most 65536 distinct members; a larger
canonical member list is rejected, because the tag domain is fixed at `u16`.
The limit applies to the flattened, deduplicated list, so combining two shapes
that each fit can still exceed it.

### Member injection

Where a value is checked against an expected type and that expected type is an
anonymous enum, a value whose type is exactly one of the members is
**injected**: it is packed into the anonymous enum with that member's tag.

```omega
open_file :: (path: *str) => enum File | IoError {
    if path.size == 0 {
        return IoError::Empty;   # injected as the `IoError` member
    }
    ...
}
```

Injection is a real conversion that constructs a tagged value, not a
reinterpretation: a member value and the anonymous enum have different
representations.

Injection is deliberately **exact and late**. The expected type is not solved as
a disjunction of its members, so the member must already be known:

- an unsuffixed numeric literal takes its ordinary default type first (`i32` or
  `f32`) and is injected only if that exact type is a member;
- an expression that needs member-specific generic inference must be given an
  explicit type by the programmer;
- a value whose type is a refined enum variant injects as its parent enum
  member, because refinement is not part of a value's representation.

Overload resolution treats injection as a conversion: a candidate that accepts
an argument exactly always outranks one that requires injecting it.

### An anonymous enum type is never inferred

An anonymous enum comes into existence only where a type is *written*: a local
annotation, a parameter or return type, an aggregate field, an alias target, or
a generic instantiation whose argument is an anonymous enum. Inference never
manufactures one, so unrelated branch or arm types are never joined into a
union:

```omega
x := if cond { a } else { b };   # error when `a` and `b` have different types;
                                 # there is no inferred `enum A | B`
```

Writing the type down makes the branches *checked* against it, injecting each
member as above:

```omega
x: enum A | B = if cond { a } else { b };
```

The same expected type reaches `match` arms, `else` blocks, function returns,
parameters, fields, and elements — every position that already checks a value
against a known type.

An anonymous enum that already exists propagates like any other type. Passing a
value of type `Errors` to `identity<T>(x: T) => T` infers `T = Errors`, because
that anonymous enum was established by `Errors`; only building a *new* one by
inference is forbidden. See [`generics.md`](generics.md).

### No subset or superset conversion

An anonymous enum does **not** implicitly convert to another anonymous enum.
`enum A | B` is not accepted where `enum A | B | C` is expected, and vice versa.
Canonical member indices and payload size can both differ, so such a conversion
would require re-tagging and possibly re-packing at runtime — a hidden cost
Omega does not introduce implicitly. Convert explicitly by matching the source
and injecting each member.

### Matching an anonymous enum

When the scrutinee is an anonymous enum, a non-catch-all arm names a **member
type** rather than an `Enum::Variant` path:

```omega
match result {
    File => { use_file(result); },
    IoError => { report(result); },
}
```

Each arm's type is resolved in the enclosing module (aliases included) and must
be exactly a member of the scrutinee's canonical member list. Naming a
non-member, or naming the same member twice, is an error.

Because the canonical list holds leaves, arms name leaves. An alias that itself
resolves to an anonymous enum is not a member and is therefore not an arm that
groups several leaves at once; match those leaf types individually.

Coverage follows the ordinary enum rules: the arms may cover every member, or
coverage is completed by a single bare `..` arm or an `else` block. Matching
compares the tag, so arms never overlap beyond the duplicate-member check.

An anonymous enum's member types are only a pattern spelling here. Elsewhere a
`match` arm keeps its ordinary meaning: enum-variant paths, literals, ranges,
and constant values are unaffected.

### Refinement to the member

Like a named enum match, an anonymous enum match refines a directly nameable
scrutinee binding for the lexical scope of the arm — including through a
pointer-to-anonymous-enum scrutinee. The proof narrows the binding to the
matched member.

Within the arm, ordinary use of that binding reads the member value: field
access, indexing, and method calls apply to the member's type, and the binding
is accepted where the member type is expected.

```omega
match value {
    *str => { print(value); print_len(value.size); },
    i32  => { print_int(value); },
}
```

The binding's storage does not change. It still holds the full anonymous enum,
so refinement never changes size, alignment, or ABI, and whole-value operations
— widening back to the anonymous enum, or taking `&mut` — use the anonymous
enum type. Exactly as for a named enum's variant refinement, the narrowed
binding is not itself an assignment target while the proof holds; see
[`bindings-and-mutability.md`](bindings-and-mutability.md).

Because an anonymous enum has no declaration, it has no methods, static
functions, or named fields of its own, and it cannot be a `conform` target:
there is nothing for a method to belong to. Everything a member offers is
reached through the refined member view inside a matching arm; behavior that
belongs to the sum itself belongs to a declared `enum`.

It is otherwise an ordinary aggregate, including at a `foreign` boundary,
where it faces the same by-value restriction every other aggregate does (see
[`foreign-function-interface.md`](foreign-function-interface.md)).

## `match`

`match` is both a statement-like control-flow construct and an expression:

```omega
kind := match value {
    Ordering::Less => "less",
    Ordering::Equal => "equal",
    Ordering::Greater => "greater",
};
```

An arm has the form:

```text
pattern => expression
pattern => { ... }
```

Arms are comma-separated and may have a trailing comma. An optional `else { ... }` follows the arm list when needed.

### Enum patterns

An enum pattern names a variant path:

```omega
match option {
    Option::None => { ... },
    Option::Some => { ... },
}
```

Enum patterns do **not** destructure or bind fields. Instead, when the scrutinee is a directly nameable local or parameter, the selected arm may refine that binding to the matched variant for the lexical scope of the arm. This makes variant-specific fields available through the same binding:

```omega
match option {
    Option::Some => {
        use(option.value);
    },
} else {
    # `option.value` is not available here unless another proof exists.
}
```

The analogous refinement applies through a pointer-to-enum scrutinee to the proven pointee variant. Refinement is lexical proof, not a permanent change to the runtime value. Taking mutable aliases can require widening as described in [`bindings-and-mutability.md`](bindings-and-mutability.md).

### Exhaustiveness and overlap

For enum matches, explicit variant arms may cover all variants. If they do not, coverage must be completed by either an `else` block or a single bare `..` catch-all arm. For an anonymous enum the same rule applies to its canonical members, with each arm naming a member type.

For value matches, Omega supports finite ordered integral domains: integer types, `bool`, and `char`. Literal and range patterns denote sets of values. Arms must partition the covered domain: two arms may not overlap. There is no first-match-wins rule for overlapping patterns.

```omega
message := match n {
    0..<100 => "less than a hundred",
    100 => "a hundred",
    .. => "more than a hundred",
};
```

Floating-point values are not supported as `match` scrutinees.

A `match` used as an expression must produce a value on every reachable arm. When static coverage proves a match exhaustive, the impossible runtime remainder is not a normal fallthrough path.

## Range patterns

The same range-expression syntax is used for match ranges, slicing, and ordinary range values:

```omega
..<b     # no written start, exclusive end
..=b     # no written start, inclusive end
a..<b    # start included, end excluded
a..=b    # both ends included
..       # open end; meaning supplied by the consuming context
a..      # explicit start with open end
```

`..<` and `..=` always require an explicit end expression. Bare `..<`, bare `..=`, `a..<`, and `a..=` are therefore invalid.

`..` is the open-ended form. Its missing bound is interpreted by the context that consumes it: for slicing, the container supplies the end; in a `match`, a bare `..` can denote the uncovered remainder.

Ranges can use any type for which Omega defines the required finite ordered integral domain. This includes `char`:

```omega
kind := match c {
    'A'..='Z' => 1,
    'a'..='z' => 2,
    '0'..='9' => 3,
} else {
    0
};
```

Standalone ranges are ordinary `core::range::Range<T>` values. Whether a `Range<T>` is iterable is determined by the normal `ToIterator`/`Iterator` protocol; see [`iteration-and-ranges.md`](iteration-and-ranges.md).

### Bare `..` catch-all

A bare `..` arm means the portion of the scrutinee domain not covered by the other arms.

For enum matches, it denotes the non-empty set of unmatched variants; for an anonymous enum, the non-empty set of unmatched members.

For numeric, `bool`, and `char` matches, the uncovered remainder must form one contiguous range. For example, removing only `0` from an integer domain leaves two disjoint ranges, so a bare `..` cannot infer a single range for that remainder:

```omega
match n {
    0 => { ... },
    .. => { ... },  # invalid: the remaining domain is split around zero
}
```

At most one bare `..` arm is permitted, and it is invalid when the other arms are already exhaustive.

## Variant-specific access and widening

The parent enum type exposes the tag, header fields, shared dynamic fields, and methods that are valid for the whole enum. Variant body fields require a refined variant type.

A refinement can come from construction or from control-flow proof such as an enum `match`. When a refined value is widened to the parent enum type, that proof is lost. Pointer/reference operations preserve or erase refinement according to the aliasing and mutability rules in [`bindings-and-mutability.md`](bindings-and-mutability.md).

For an anonymous enum, the parent type exposes nothing of its own: the member
value requires a refined member type, obtained by matching. Widening a refined
member back to the anonymous enum discards the proof without changing the
value.
