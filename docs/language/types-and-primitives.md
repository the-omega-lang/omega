# Types and primitives

This chapter is normative for current Omega language behavior. Known implementation limitations are tracked under [`../issues/`](../issues/).

## Primitive types

```text
void  never  bool  char
i8 i16 i32 i64 isize
u8 u16 u32 u64 usize
f32 f64
```

- `i8`/`u8`, `i16`/`u16`, `i32`/`u32`, and `i64`/`u64` have their named widths.
- `isize` and `usize` have the target pointer width.
- `f32` and `f64` are 32-bit and 64-bit floating-point types.
- `char` stores a Unicode scalar value and occupies 4 bytes.
- `bool` has the values `true` and `false`.
- `void` is a zero-sized value type.
- `never` is permitted only as a function-like declaration's return type and denotes divergence.

Primitive types may receive methods through `primitive Type { ... }`; see [`specs-and-conformance.md`](specs-and-conformance.md).

## Numeric literal inference

An unsuffixed numeric literal first uses an expected type when one is available and compatible with the literal's family. Integer-looking literals do not silently become floating-point values and floating-point-looking literals do not silently become integers.

With no compatible expected type:

```omega
n := 5;        # i32
x := 1.0;      # f32
```

An explicit suffix fixes the type:

```omega
x := 5u8;
y := 1.0f64;
```

The integer suffixes are `i8`, `i16`, `i32`, `i64`, `isize`, `u8`, `u16`, `u32`, `u64`, and `usize`. Floating suffixes are `f32` and `f64`.

For C variadic calls, arguments in the variadic tail undergo the C default argument promotions (including `f32` to `f64` and narrow integers to the C-compatible promoted integer width). Current implementation limitations are listed under [`../issues/`](../issues/).

## `never`

`never` may be written only as the declared return type of a function, method, foreign declaration, or gap function:

```omega
foreign(c) exit(code: i32) => never;

spin_forever() => never {
    loop { }
}
```

A declaration returning `never` must not complete normally. A diverging expression is compatible with any expected expression type because it never produces a value. Calls to `never`-returning functions and `loop` expressions with no reachable `break` are therefore usable in otherwise value-producing contexts.

`never` is not a storable value type: it is invalid as a local/field/parameter type, generic argument, or aggregate member type.

A `foreign` declaration returning `never` is a contract with foreign code. If that foreign function returns, program behavior is invalid.

## Pointer, array, and slice type forms

Omega distinguishes these forms:

```text
*T          immutable thin pointer to T
*mut T      mutable thin pointer to T
[N]T        fixed-size array value containing N elements
*[N]T       pointer to a fixed-size array
[]T         inferred-size array syntax; legal only in the declaration case below
*[?]T       thin pointer to unknown-size array storage
*mut [?]T   mutable form of the same
*[]T        immutable slice (fat pointer)
*mut []T    mutable slice
*str        immutable string fat pointer
*mut str    mutable string fat pointer
```

`[?]T` cannot exist as a standalone value. Its valid use is behind a pointer (`*[?]T` / `*mut [?]T`).

A bare `*T` points to one value and is not indexable as an arbitrary array. `*[?]T` is still thin and stores no length, but it may be indexed and may be range-sliced when an explicit end is available.

`*mut T` implicitly widens to `*T` at ordinary coercion sites; the reverse direction is not implicit. The same directional mutability rule applies to `*mut [?]T` → `*[?]T` and mutable slices → immutable slices.

## Slice, string, and dynamic-spec-object ABI shapes

The following source-level types are distinct even though each has a two-word logical representation:

- `*[]T` / `*mut []T`: data pointer plus an `i32` element count, exposed as `.length`.
- `*str` / `*mut str`: data pointer plus an `i32` UTF-8 byte count, exposed as `.size`.
- `spec *S` / `spec *mut S`: data pointer plus a dispatch-table pointer.

There is no implicit coercion between `*str` and `*[]u8`; conversions are explicit. Dynamic spec objects are governed by [`specs-and-conformance.md`](specs-and-conformance.md).

These representation details are part of Omega's current observable ABI model. Compiler-specific lowering details belong in [`../architecture/abi-and-representation.md`](../architecture/abi-and-representation.md).

## Unknown-size array pointers

`*[?]T` and `*mut [?]T` represent an address at which zero or more `T` values may be stored, without carrying a length.

```omega
sum(values: *[?]i32, count: usize) => i32 {
    mut total := 0;
    mut i : usize = 0;
    for ; i < count; i += 1 {
        total += values[i];
    }
    total
}
```

Because no length is stored, slicing one requires an explicit end. A raw `*T` may be explicitly cast to `*[?]T` when the programmer knows the address denotes array storage.

## Inferred array length

`[]T` is not a general standalone type. It is accepted only as the explicit type of a declaration whose initializer is an array literal:

```omega
bytes : []u8 = [10, 20, 30];
```

The declaration's resulting type is `[3]u8`. If the initializer is not an array literal, or its elements cannot satisfy `T`, the declaration is invalid.

## `char`

`char` comparisons use Unicode scalar/codepoint ordering. It supports `==`, `!=`, `<`, `<=`, `>`, and `>=`, and may be used in match ranges.

The checked constructor `char::from_u32` rejects values above `0x10FFFF` and UTF-16 surrogate values. Iteration through `char` values skips the surrogate range.

`char` does not directly support arithmetic, bitwise arithmetic, or unary `~`. Cast to a numeric type when codepoint arithmetic is intended.

A `char` or `bool` can be explicitly cast to numeric types. `u8` can be cast to `char`; arbitrary wider integers cannot be cast directly to `char` and should use `char::from_u32`. No numeric type casts directly into `bool`.

## `bool`

`bool` supports:

- equality: `==`, `!=`
- eager boolean bitwise operations: `&`, `|`, `^`
- short-circuit operations: `&&`, `||`
- unary negation: `!`

It does not implicitly become an integer and does not support arithmetic, ordering, shifts, or unary `~`.

## Pointer arithmetic and comparison

Pointer arithmetic is byte-oriented. Pointer-plus/minus-integer operations are unscaled; they do not multiply the integer by the pointee size. Pointer arithmetic produces `usize` rather than a new pointer implicitly, so explicit casts are used where a pointer result is intended.

Pointer equality/comparison is address-based. Pointer subtraction is defined; pointer-plus-pointer is not.

## Layout

Fixed arrays store elements inline. Struct fields are laid out in declaration order. Unions place every field at offset zero and occupy enough bytes for the largest field. Enums use the layout defined in [`enums-and-pattern-matching.md`](enums-and-pattern-matching.md).

Structs and enums are packed by default: the current default introduces no natural-alignment padding. `@layout(pack = n, align = n)` can request explicit packing/alignment behavior on supported declarations. This packed-default model has target-safety caveats tracked in [`../issues/design-debt.md`](../issues/design-debt.md).

`sizeof<Type>` evaluates the size, in bytes, of the specified type according to this layout model. See [`annotations-and-sizeof.md`](annotations-and-sizeof.md).
