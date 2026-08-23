# Strings, casts, arrays, and slices

This chapter is normative for current Omega language behavior. Known implementation limitations are tracked under [`../issues/`](../issues/).

## Strings and byte strings

An ordinary string literal has type `*str`:

```omega
s : *str = "hello";
```

`*str` is a fat pointer containing a data address and an `i32` UTF-8 byte count. The count is available as `.size`. Strings are not implicitly null-terminated.

A byte-string literal has type `*[]u8`:

```omega
bytes : *[]u8 = b"hello";
```

Byte strings are also not implicitly null-terminated. C APIs expecting a NUL-terminated `*u8` therefore commonly use an explicit terminator and cast:

```omega
foreign(c) puts(s: *u8) => i32;
puts(<*u8>b"hello\0");
```

`*str`, `*[]u8`, and `*u8` are distinct types. `*str` and `*[]u8` are fat pointers; `*u8` is thin. There is no implicit string/slice coercion merely because their data representation is compatible.

## Explicit casts

A cast is a prefix expression:

```omega
<i64>some_i32
<*[]u8>some_string
<*u8>some_slice
```

Numeric casts explicitly convert among supported numeric types. Integer extension/truncation and float/integer conversion use the source and destination signedness/widths. Float-to-integer conversion is saturating rather than trapping.

Pointer/integer casts use the target pointer width. A cast to a mutable pointer requires a source that is allowed to yield mutable access; a cast cannot silently manufacture mutability from an immutable source.

Pointer-to-pointer casts are explicit reinterpretations. Pointee types need not be identical.

### Casts into an anonymous enum

A cast to an anonymous enum is not a reinterpretation. It writes the destination type down, which is the one thing conversion into an anonymous enum requires, and then performs the same conversion an expected type would:

```omega
member := <enum i32 | *str>10;    # tagged as the `i32` member
small : enum A | B = A{};
large := <enum A | B | C>small;   # re-tagged for the wider shape
```

The rule is the one in [`enums-and-pattern-matching.md`](enums-and-pattern-matching.md): the cast succeeds when every type the source could hold is a member of the target. So `<enum A | B | C>small` is valid while `<enum A | B>large` and `<enum A | C>small` are not — casting adds no narrowing, no runtime-checked downcast, and no member extraction. Use `match` to reach a single member's value.

Because the cast itself establishes the type, no surrounding expected type is needed: `x := <enum A | B>A{};` gives `x` that anonymous-enum type, while an untyped `if`/`match` over unrelated branch types remains an error.

## Fat-pointer casts

The following explicit conversions are supported:

- fat pointer → compatible fat pointer: reinterpret the data/count representation, for example `*str` ↔ `*[]u8`;
- fat pointer → thin pointer: keep the data address and discard the count, for example `<*u8>slice`;
- pointer to `[N]T` → `*[]T`: keep the data address and use `N` as the resulting slice length.

A bare thin `*T` cannot be cast directly to `*[]T`, because no length exists to place in the resulting slice. It can be reinterpreted as `*[?]T`, then explicitly sliced with a runtime range to construct a real slice.

```omega
p : *mut i32 = &mut first;
array_ptr := <*mut [?]i32>p;
slice := &array_ptr[0..<count];
```

## Fixed arrays

An array literal creates a fixed-size array value when used in an ordinary expression/type context:

```omega
values : [3]i32 = [1, 2, 3];
```

`[N]T` stores `N` values inline. `[]T` may be used on a declaration with an array-literal initializer to infer `N`:

```omega
values : []i32 = [1, 2, 3];   # resulting type: [3]i32
```

See [`types-and-primitives.md`](types-and-primitives.md) for the complete array/pointer type forms.

## Compile-time slice literals

`&[...]` constructs a slice whose elements are compile-time data:

```omega
names := &["alice", "bob", "carol"];
```

The resulting value is a fat slice pointer. This spelling is distinct from a bare array literal: `[a, b, c]` is an inline fixed array, while `&[a, b, c]` denotes a slice referring to compile-time-stored elements.

`&[...]` is also the required slice-literal form in compile-time aggregate positions such as enum header values; a bare `[...]` there does not mean a slice.

## Indexing and slicing

Fixed arrays, slices, strings, and unknown-size array pointers support their applicable indexing operations. A range slice uses the range syntax from [`iteration-and-ranges.md`](iteration-and-ranges.md):

```omega
part := &items[start..<end];
rest := &items[start..];
prefix := &items[..<end];
```

A slice operation produces a fat pointer. Mutability of the produced slice follows the mutability of the source place/pointer.

A `*[?]T` carries no length, so an omitted end cannot be inferred; slicing it requires an explicit end. A bare `*T` is a single-value pointer and is not directly indexable/sliceable as array storage.

Inside an indexing expression, an omitted range bound is interpreted relative to the container. A standalone range expression has the independent range semantics specified in [`iteration-and-ranges.md`](iteration-and-ranges.md).

## C string interoperability

Because `*str` is not NUL-terminated, dropping its length and passing the data pointer to a C `%s` API is not generally safe. Length-aware C interfaces should receive both the data pointer and the byte count; for `printf`, `%.*s` is the conventional form.
