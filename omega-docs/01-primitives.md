# Primitives & representation

## The type set

```
void  bool  char
i8 i16 i32 i64 isize
u8 u16 u32 u64 usize
f32 f64
```

`char` is a decoded 4-byte Unicode scalar value (Rust's `char`, not a byte —
that's `u8`). `isize`/`usize` are the only two primitives whose IR type is
genuinely target-dependent (`codegen.pointer_type()`); every other numeric
type is a hardcoded width regardless of target, matching this compiler's
current single-target-assumption reality noted throughout `numeric_kind`/
`cast_class`'s own doc comments.

## Codegen representation

Every `ResolvedType` lowers to a flat list of Cranelift IR leaves
(`IntoIRType::into_ir_type`). A struct/enum-typed value passed as a
parameter or held in a register-backed local is literally that leaf list —
not a single aggregate IR type — which is what makes struct-by-value
parameter passing and the whole `Storage::Parameter` model work without a
real C-ABI aggregate-passing implementation (see the caveat at the bottom).

| Type | Leaves |
|---|---|
| `void` | *(none)* |
| `bool` | `i8` (Cranelift has no boolean type; `0`/`1`) |
| `char` | `i32` (a decoded scalar, not a byte) |
| `i8`/`u8` | `i8` |
| `i16`/`u16` | `i16` |
| `i32`/`u32` | `i32` |
| `i64`/`u64` | `i64` |
| `isize`/`usize` | the target's pointer type |
| `f32` / `f64` | `f32` / `f64` |
| `*T` / `*mut T` | one thin pointer |
| `[T]` (decayed array param, e.g. `argv: [*u8]`) | one thin pointer, **no length** |
| `[T; N]` (`SizedArray`) | `N` copies of `T`'s own leaves, inline, no indirection |
| struct | each field's leaves, back to back (+ padding, see `@layout`) |
| union | one opaque run of `i32`/`i8` chunks sized to the largest member |
| enum | `[tag][header][dynamic fields][payload]`, each region flattened the same way |
| `spec *T` (dynamic dispatch) | two pointers |

## Fat pointers

Three distinct types share the identical two-leaf runtime shape
`[data_ptr, len_or_vtable_ptr]`, but are never interchangeable at the type
level (no implicit coercion between them):

- **`*[T]` (`Slice { item, mutable }`)** — `[data pointer, i32 length]`.
  This is *not* the same as `*T` to an array — `Context::resolve_type`
  special-cases `*[T]` specifically so it never becomes `Pointer(Array(T))`.
  A **bare** `[T]` (no leading `*`) is a different, older, unsized shape:
  a single thin pointer with no length at all, used only for C-style decayed
  array parameters (`argv: [*u8]`) — a deliberate, narrower legacy case, not
  a slice.
- **`*str` (`Str { mutable }`)** — the exact same `[data ptr, i32 length]`
  shape as `*[u8]`, but a fully separate nominal type: no implicit coercion
  to/from `Slice`/`Pointer` in either direction, and never null-terminated.
  See [strings, casting & slices](11-strings-casting-and-slices.md) for why
  it exists and how the two families interconvert explicitly.
- **`spec *T` (`SpecObject { spec, type_args, mutable }`)** — `[data
  pointer, vtable pointer]`, Omega's dynamic-dispatch trait-object pointer.
  See [specs](08-specs.md).

All three fell out of the *same* codegen mechanism: every Omega call
already compiles to `call_indirect` (there is no direct-call instruction at
all in this backend), and static data blobs with pointer relocations were
already one API call away once slices existed — so `spec *T` needed no new
low-level machinery, only a new 2-leaf type and a vtable-building pass.

## `char`: comparable, but not arithmetic

```
if c >= 'A' { if c <= 'Z' { true } else { false } } else { false }

classify(c: char) => *u8 {
    match c {
        'A'...'Z' => <*u8>b"upper\0",
        'a'...'z' => <*u8>b"lower\0",
        '0'...'9' => <*u8>b"digit\0",
    } else { <*u8>b"other\0" }
}
```

`char` supports the full comparison family (`== != < <= > >=`), ordered by
raw codepoint, and can be used as a `match` scrutinee — including range
patterns (`'A'...'Z'`), the same shared range grammar
[ranges elsewhere](05-enums-and-pattern-matching.md) use. `char`'s
`integer_domain()` (what `match` exhaustiveness treats as "the whole
domain") is `0..=0x10FFFF` (`char::MAX`) — the same real range Rust's
`char` occupies. This doesn't carve out the surrogate hole
(`0xD800..=0xDFFF`), which is sound rather than an oversight: a `char`
literal is always validated through `char::from_u32` at parse time, so no
real `char` value can ever land in that hole in the first place — the
interval-exhaustiveness checker just doesn't need to know it exists.

**`char` still has *no* arithmetic, bitwise, or cast support** — this is
deliberate, not the same gap as before. Comparison only reads a `char`'s
underlying scalar value; arithmetic would let you *construct* one
(`'a' + 1`), and there's no validation step anywhere in this language yet
that would stop the result from being an unassigned codepoint or landing
inside the surrogate hole — a `char` that isn't a valid Unicode scalar
value at all, silently violating the type's own invariant. Whether/how to
support this safely (e.g. a fallible `char::from_u32`-style constructor
instead of raw arithmetic) is intentionally left as **future work** rather
than solved narrowly here — noted so it doesn't get silently
"fixed" by just widening `numeric_kind` later without thinking through the
validity question again.

## Layout, packing, and `sizeof`

Struct/enum fields are **packed by default** (no implicit alignment padding
at all — x86_64 tolerates unaligned loads/stores, so this is safe but not
C-ABI-compatible). `@layout(pack = n, align = n)` (see
[annotations](09-annotations.md)) is the only way to introduce padding, and
`type_alignment` is the *only* source of alignment anywhere in the layout
model — never inferred from a primitive's own natural width. `sizeof<Type>`
is a real expression, computed the same way `total_bytes` sums any type's
own leaf sizes.

## Caveats

- **No real C-ABI aggregate-passing convention.** Structs/enums are passed
  as flattened positional scalars, not per platform calling-convention
  aggregate rules. This works fine for Omega-to-Omega calls (including
  across separately-compiled `--extern` object files, since both sides
  agree by construction) but means an Omega function taking a struct
  by value is not safely callable from, say, hand-written C expecting the
  System V ABI's actual struct-passing rules.
- **Two confirmed, unfixed variadic-`f64` codegen bugs**, both narrower than
  "floats are broken" — plain local `f64` variables work fine through
  `printf`-style varargs at any optimization level:
  - An `f64` **function parameter** forwarded directly into a variadic call
    prints `0.0` (any `-O` level).
  - An `f64` read via an **enum body-field projection** and passed to a
    variadic call prints garbage, but only under `-O1` and above (`-O0` is
    fine).

  Both are plausibly the same root cause (this codegen's variadic-argument
  ABI setup mishandling a float sourced from something other than a plain
  local), not confirmed to be identical. Neither is fixed; both are worked
  around in example code by not exercising the shape.
- **`isize`/`usize` width is target-dependent by design** — nothing in
  `omega-core` bakes in a `min_value`/`max_value` bound for them, since any
  literal bound would silently be wrong on a target this toolchain wasn't
  built assuming.
