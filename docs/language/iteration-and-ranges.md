# Iteration and ranges

This chapter is normative for current Omega language behavior. Known implementation limitations are tracked under [`../issues/`](../issues/).

## Range expressions

Omega has three range operators:

```omega
1..<10    # end-exclusive
1..=10    # end-inclusive
1..       # omitted end
```

An omitted start uses the same contextual `..` spelling:

```omega
..<10
..=10
..
```

`a..b` and `..b` are invalid: if an end is written, the source must state whether that end is excluded (`..<`) or included (`..=`).

In ordinary expression position, a range denotes `core::range::Range<T>` and can be stored, copied, passed, and returned:

```omega
r := 1..<10;
for value in r { ... }
```

A range value stores its bounds/inclusiveness, not iteration state.

## Inferred range bounds

The meaning of an omitted bound depends on context:

| Context | Omitted bound |
|---|---|
| ordinary range expression | inferred from `T`'s range domain (`Bounded`) |
| indexing/slicing | inferred from the container's bounds/length |
| match pattern | inferred from the unmatched portion of the scrutinee domain |

Container/match context takes precedence over ordinary range-domain inference. Therefore `&items[5..]` means “from index 5 through the end of `items`”, not “through `usize::MAX`”.

A stored range cannot later mean “the rest of whichever container uses me”; its inferred bounds were determined when the range value was formed.

Bare `..` has no standalone type source and therefore requires context that determines the missing bounds.

## Range protocols

`Range<T>` may be formed for any `T` for which its bounds can be typed. Iterating a stored range requires the core range protocols:

- `Successor`: supplies a checked next value;
- `Ord`: supplies the equality/ordering needed to recognize inverted ranges and the final element;
- `Bounded`: required when a range endpoint must be inferred from the type's domain.

The core integer types provide the required ordering, successor, and bounded-domain conformances. `char` also participates and its successor skips the UTF-16 surrogate interval. Floating-point types do not implement this discrete successor protocol.

An inclusive range must terminate safely at the maximum representable value without computing an overflowing `max + 1`.

## `for ... in` grammar

```omega
for value in source { ... }
for mut value in source { ... }
for value : SomeType in source { ... }
```

The loop binding is one identifier, optionally `mut` and optionally explicitly typed. Omega has no destructuring loop-binding syntax.

This form is distinct from the classic three-clause loop:

```omega
for init; condition; post { ... }
```

## Iterator protocol

The standard protocol is defined by core specs equivalent to:

```omega
spec Iterator<T> {
    next(*mut self) => Option<T>;
}

spec ToIterator<T> {
    to_iterator(*self) => spec Iterator<T>;
}
```

A value used in `for x in value` must nominally conform to an applicable `ToIterator<T>` or `Iterator<T>`; merely having same-named methods is insufficient.

If `ToIterator<T>` is available, it is preferred and `to_iterator()` supplies the cursor. Otherwise a value that already conforms to `Iterator<T>` is itself the cursor.

If several `ToIterator<T>` conformances make the element type ambiguous, an explicit loop-binding type can select the intended one:

```omega
for value : u8 in source { ... }
```

`ToIterator<T>::to_iterator` returns `spec Iterator<T>` in the static-dispatch sense: each implementor may return its own concrete iterator type satisfying `Iterator<T>`. Consequently `ToIterator<T>` is not object-safe as a dynamic `*spec ToIterator<T>` contract; see [`specs-and-conformance.md`](specs-and-conformance.md).

## Semantic expansion of protocol iteration

A protocol-driven loop behaves as if it repeatedly calls the iterator and matches its `Option<T>` result:

```omega
{
    mut $iter := source.to_iterator();
    while true {
        $next := $iter.next();
        match $next {
            Option::None => { break; }
            Option::Some => {
                value := $next.value;
                # original loop body
            }
        }
    }
}
```

When `source` is already an `Iterator<T>`, `$iter` is initialized directly from `source` instead of calling `to_iterator`.

The user loop binding is freshly assigned each iteration. If written `mut`, mutating it does not mutate the iterator's internal cursor unless the loop body separately accesses the source.

`break` and `continue` target the `for ... in` loop normally.

## Range literals in `for ... in`

A range written directly in the iterator position is an ordinary `Range<T>` value and follows the same `ToIterator`/`Iterator` protocol as a stored range:

```omega
for i in 0..<10 { ... }
for i in 0..=10 { ... }
for c in 'a'..='z' { ... }
```

There is no separate source-level range-loop semantic path. Consequently the element/domain rules are those of `Range<T>` and its `Successor`/`Bounded` conformances, including support for `char`. Generated-code quality for this protocol-based form is tracked separately under [`../issues/known-issues.md`](../issues/known-issues.md).
