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

For enum matches, explicit variant arms may cover all variants. If they do not, coverage must be completed by either an `else` block or a single bare `..` catch-all arm.

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

For enum matches, it denotes the non-empty set of unmatched variants.

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
