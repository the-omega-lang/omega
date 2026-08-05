# Primitives & representation

## The type set

```
void  never  bool  char
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

`never` isn't a type in the same sense the rest of this list are — see
"`never`: not a conventional type" below.

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
| `never` | *(none)* -- see below |
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

## `never`: not a conventional type

`never` is a function/method/extern/gap's own declared return type,
meaning "this doesn't return" (Rust's `!`):

```
extern exit : (code: i32) => never;

spin_forever() => never {
    loop {
        # ...
    }
}
```

It's legal in exactly that one position — a return type — and rejected
everywhere else a type could be written (a local's type, a struct/union/
enum field, a parameter, a generic argument): there is no such thing as a
`never`-typed *value* to store anywhere, only a function position that's
provably unreachable if reached at all. `x : never;`/`bad(p: never) => void
{ }` are both compile errors (`'never' is only allowed as a function/
method's own return type`); `x := exit(1);` compiles (the local's type is
inferred, not written), but is immediately unreachable code itself, same
as anything else following a call that never returns.

A body declared `=> never` has to actually diverge — end in a `loop { }`
with no way out (see [control flow](03-control-flow.md)'s "Loops"), or a
tail call/`return` reaching a `never`-returning function itself — `foo()
=> never { }` is rejected exactly like a type mismatch, because that's
what it is (an empty body's effective type is `void`, not a proof of
divergence). This falls out of the same machinery that already lets a
fully-diverging `if`/`else` or a bare `return`/`break`/`continue` satisfy
*any* expected type (`Analyzer::block_type`'s `None` — "diverges,
compatible with anything," Rust's `!` in everything but name) — a call to
a `never`-returning function, and a `loop` with no `break` targeting it,
are just two more cases that machinery recognizes, not a new, separately-
threaded type flowing through ordinary coercion. One real consequence of
that design: the compiler traps (`Unreachable`, a hard runtime fault) if
a function actually declared `=> never` somehow returns anyway — the
same backstop LLVM emits after any `noreturn` call, for the same reason
(an `extern`'s `never` is trusted, not verified, the same as every other
part of its signature).

See [gaps and glue](21-gaps-and-glue.md) for `never`'s motivating use —
`exit`-style platform capabilities that genuinely don't return — and
[control flow](03-control-flow.md) for `loop`, its natural companion.

## Fat pointers

Three distinct types share the identical two-leaf runtime shape
`[data_ptr, len_or_vtable_ptr]`, but are never interchangeable at the type
level (no implicit coercion between them):

- **`*[T]` (`Slice { item, mutable }`)** — `[data pointer, i32 length]`,
  read via `.length` (a genuine element count).
  This is *not* the same as `*T` to an array — `Context::resolve_type`
  special-cases `*[T]` specifically so it never becomes `Pointer(Array(T))`.
  A **bare** `[T]` (no leading `*`) is a different, older, unsized shape:
  a single thin pointer with no length at all, used only for C-style decayed
  array parameters (`argv: [*u8]`) — a deliberate, narrower legacy case, not
  a slice.
- **`*str` (`Str { mutable }`)** — the exact same `[data ptr, i32 byte
  count]` shape as `*[u8]`, but a fully separate nominal type: no implicit
  coercion to/from `Slice`/`Pointer` in either direction, and never
  null-terminated. Read via `.size`, deliberately *not* `.length` — a
  `*str`'s second leaf is a UTF-8 *byte* count, not a character count, and
  "length" would nudge a reader toward the latter. Both `.length` and
  `.size` compile to the identical projection (`CheckedProjection::
  SliceLength`) and codegen (reads the same second leaf) — only the
  surface name a user spells it with differs by type (see
  `Analyzer::project_slice_field`). See [strings, casting &
  slices](11-strings-casting-and-slices.md) for why `*str` exists and how
  the two families interconvert explicitly.
- **`spec *T` (`SpecObject { spec, type_args, mutable }`)** — `[data
  pointer, vtable pointer]`, Omega's dynamic-dispatch trait-object pointer.
  See [specs](08-specs.md).

All three fell out of the *same* codegen mechanism: every Omega call
already compiles to `call_indirect` (there is no direct-call instruction at
all in this backend), and static data blobs with pointer relocations were
already one API call away once slices existed — so `spec *T` needed no new
low-level machinery, only a new 2-leaf type and a vtable-building pass.

## `char`, `bool`, and pointer arithmetic

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

`char`, `bool`, and every pointer type (`*T`/`*mut T`) also support
arithmetic/bitwise ops (`+ - * / % & | ^ << >>`, and unary `~`) — each
non-numeric operand implicitly **coerces** to a real numeric type first
(`ResolvedType::arithmetic_repr`): `char` to `u32`, a pointer to `usize`.
The **result is that numeric type, never cast back implicitly** —
`some_char + 1` is a `u32`, not a `char`, and `some_char += 1` still
doesn't type-check (there's no implicit path back into `char`). This is
what keeps arithmetic sound despite `char` having no validating
constructor yet (see below): there is no way for it to ever produce an
invalid codepoint *as a `char`* — only ever more arithmetic on a plain,
unconstrained `u32`. `char + char` and `pointer + pointer` are both
allowed (not unsound, just unusual) — the result's different type from
either operand is itself already a strong signal that something numeric,
not "`char`-shaped", happened. A pointer coerces even for `==`/`!=`, which
is what makes comparing a `*mut T` against a `*T` type-check for free:
both sides become a plain `usize`, so pointee type and mutability never
enter the comparison at all.

`bool` is the one exception that stays **native**, uncoerced: `== != & |
^` all work directly on `bool`, producing `bool`, since `bool` is *closed*
under all five (any combination of `0`/`1` is still `0`/`1`). Arithmetic
and shifts are still not offered on `bool` (`true + true` has no meaning
to fall back on), and neither is unary `~` (bitwise-NOT of `bool`'s `0`/`1`
representation does *not* stay within `{0,1}` the way `& | ^` do). A
logical-not (`!`) operator to complete this story doesn't exist in the
language at all yet — see the caveat below.

Casting follows the same asymmetry Rust's own `as` does: `char`/`bool`
both cast *out* to any numeric type freely, but only one direction casts
back *in* — `u8 -> char` (every byte is a valid codepoint) — and nothing
at all casts into `bool` (no implicit "nonzero is true"). Any other
integer into `char` (an arbitrary `u32`, say) is still rejected: this
compiler has no fallible/validating constructor yet
(`char::from_u32`-equivalent) to catch a codepoint that isn't a valid
Unicode scalar value, and a plain cast allowing it would silently violate
`char`'s own invariant. That's **future work**, not solved narrowly here —
noted so it doesn't get silently "fixed" by just widening the cast rules
later without thinking through the validity question again.

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
- **A floating-point argument passed to a variadic call
  (`printf`-style) is unreliable — not an Omega bug, a genuine Cranelift
  gap.** The x86-64 System V ABI requires the *caller* of a variadic
  function to set `%al` to the number of vector (XMM) registers used for
  floating-point arguments, before the call — that's how `printf` itself
  knows whether to read a `%f` argument from a register or the stack.
  `cranelift-codegen` (confirmed directly against its own source, version
  0.131.1) has no code anywhere handling this — no `ArgumentPurpose`
  variant, nothing matching `vararg`; it's not a partial or buggy
  implementation, there simply isn't one. This isn't unique to this
  compiler: `rustc_codegen_cranelift` hit the identical wall and
  resolved it by forbidding float arguments in variadic calls outright
  rather than working around it — see [its own tracking
  issue](https://github.com/rust-lang/rustc_codegen_cranelift/issues/1451).

  Since nothing ever sets `%al`, a variadic call with a float argument
  reads whatever garbage happens to be sitting in `%rax`'s low byte at
  that program point — inherently undefined, not tied to any one
  language-level shape. Confirmed (via `--emit=asm`, reading the actual
  generated instructions) on: an `f64` function parameter forwarded
  directly into a variadic call; an `f64` read via an enum body-field
  projection; and a plain local `f64`/`f32` inside a large enough
  function — the last of these previously believed safe "at any
  optimization level," which turned out to only have been true for
  small functions, where whatever's left in `%al` happens to still work
  out. All are almost certainly the same single root cause (garbage in
  `%al`) surfacing under different register-allocation outcomes, not
  several independent bugs. Not fixed — no clean fix is available until
  Cranelift itself grows real vararg support, or this compiler grows its
  own workaround (rejecting the shape at compile time, or routing such
  calls through a small correctly-ABI'd shim, both still open).
- **There is no `!` (logical-not) operator at all.** `bool` gets native
  `== != & | ^` (see above), which covers *combining* two `bool`s, but
  negating one still has no spelling — adding `!` is a real, if small,
  language feature (a new parser token plus a new `Expression`/`HirExpr`/
  `CheckedExpr`/`MirExpr` variant, each needing its own codegen arm), not
  an analyzer-only change like the rest of this section, so it's left as
  deliberate future work rather than folded in here.
- **`isize`/`usize` width is target-dependent by design** — nothing in
  `core` bakes in a `min_value`/`max_value` bound for them, since any
  literal bound would silently be wrong on a target this toolchain wasn't
  built assuming.
