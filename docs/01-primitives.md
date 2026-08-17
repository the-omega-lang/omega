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
| `*[?]T` / `*mut [?]T` (`Array`) | one thin pointer, **no length** |
| `*[]T` / `*mut []T` (`Slice`) | `[data pointer, i32 length]` |
| `[N]T` (`SizedArray`) | `N` copies of `T`'s own leaves, inline, no indirection |
| struct | each field's leaves, back to back (+ padding, see `@layout`) |
| union | one opaque run of `i32`/`i8` chunks sized to the largest member |
| enum | `[tag][header][dynamic fields][payload]`, each region flattened the same way |
| `spec *T` (dynamic dispatch) | two pointers |

## Number literal defaults

An unsuffixed number literal takes its type from context when there is one —
the expected type of the parameter, binding, field or operand it appears in —
adapting only within its own family (a literal written without a fractional
part never silently becomes a float, or vice versa). An explicit suffix
(`5u8`, `1.0f64`) always wins outright and is never inferred over.

With no context to adapt to, the defaults are **`i32`** and **`f32`**:

```
n := 5;            # i32
x := 1.0;          # f32
y := 1.0f64;       # f64, written explicitly
z : f64 = 1.0;     # f64, adapted from the annotation
```

The float default is deliberately `f32`, unlike C's `double` and Rust's
`f64`. Omega treats embedded as a first-class target, and the FPUs on the
parts it aims at — Cortex-M4F, M7 and similar — are single-precision only.
An unsuffixed `1.0` meaning `f64` there quietly pulls in software emulation:
a large, slow call where the source looks like arithmetic. That is precisely
the invisible cost this language exists to refuse, so the default is the one
the hardware can actually execute, and double precision is opt-in.

This does not change C interop. An `extern` with a prototype passes an `f32`
as a 4-byte `float`, because the prototype fixes the width.

The one exception is a **variadic** argument, and that is C's rule rather
than Omega's: a variadic callee has no prototype for its `...` arguments, so
it cannot know what width arrived — `printf` decides how many bytes to read
purely from its format string. C fixes the widths at the call site instead
(*default argument promotions*, C11 §6.5.2.2): anything narrower than `int`
becomes `int`, and `float` becomes `double`. Omega does the same, on the
variadic tail only (`promote_variadic_arg`). It is why `%f` reads both
`float` and `double` in C, and why passing a bare 4-byte `f32` there would
make `printf` read 8 bytes of garbage.

Float variadic arguments are separately broken today for an unrelated
reason — see [known issues](14-known-issues.md).

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

- **`*[]T` (`Slice { item, mutable }`)** — `[data pointer, i32 length]`,
  read via `.length` (a genuine element count).
  This is *not* the same as a pointer to an unsized array — `Context::
  resolve_pointer_type` gives `*[]T` its own dedicated production so it
  never becomes `Pointer(Array(T))`. `*[?]T` (no `?`) is a different,
  lengthless shape — see "`*[?]T`: a pointer with array-like properties"
  below.
- **`*str` (`Str { mutable }`)** — the exact same `[data ptr, i32 byte
  count]` shape as `*[]u8`, but a fully separate nominal type: no implicit
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

## Array/slice/pointer syntax, all six shapes

```
[N]T     # array of N items, by value (SizedArray)
*[N]T    # pointer to that -- no special handling, an ordinary Pointer
[]T      # inferred-size array -- valid only on a typed array-literal declaration
*[?]T     # pointer to an unsized array -- indexable, sliceable (Array)
[?]T     # unsized array -- invalid on its own
*[]T    # slice -- fat pointer, [data, length] (Slice)
```

`[N]T` and `*[N]T` need no special handling at all: a sized array is an
ordinary value type (see "Layout, packing, and `sizeof`" below), and a
pointer to one is just `Pointer { pointee: SizedArray(T, N), .. }`, the
same as a pointer to anything else. `[]T` and `[?]T` are never legal
standalone — `Context::resolve_type` rejects both unconditionally wherever
written directly, since neither has a length to give a value. `[?]T`'s
only legal use is behind a leading `*` (`*[?]T`, an unsized-array pointer).
`[]T` has one additional legal use, covered in its own section below; behind
a leading `*` it is a slice (see "Fat pointers" above).

## `*[?]T`: a pointer with array-like properties

```
sum(argv: *[?]i32, count: usize) => i32 {
    mut total := 0;
    mut i : usize = 0;
    for ; i < count; i += 1 {
        total += argv[i];      # indexing works directly
    }
    return total;
}

p : *mut i32 = &mut some_local;
arr := <*mut [?]i32>p;           # explicit cast, either direction
s := &arr[0..<count];           # slicing works too
```

`*[?]T` (`ResolvedType::Array`) is genuinely just a thin pointer value (one
leaf, no length) with two added capabilities a bare `*T` doesn't have:
indexing (`arr[i]`) and range-slicing (`&arr[a..<b]`, building a real,
bounded `*[]T`). Plain single-element indexing (`ptr[i]`) and
range-slicing (`&ptr[a..<b]`) still don't work on an ordinary `*T`/`*mut
T` — `*T` is strictly a single-value pointer; the only way to get
array-ness out of one is to cast it to `*[?]T` first. That line is drawn
structurally, not by convention: `Analyzer::project_index`'s whitelist
never includes a bare `Pointer`, only `Array`/`SizedArray`/`Slice`/`Str`,
and `Analyzer::analyze_slice`'s base-type match doesn't include `Pointer`
either.

**Mutability is a type-level fact**, exactly like `*T`/`*mut T` — `*[?]T`
(immutable) vs. `*mut [?]T` (mutable, the `mut` sits on the pointer sigil,
same as everywhere else — never inside the brackets). Whether `arr[i] = x`
is legal follows `*[?]T`'s own declared mutability, never whatever binding
happens to hold the value — the same directional rule a real pointer
already enforces. `*mut [?]T` widens to `*[?]T` implicitly at ordinary
coercion sites, mirroring `*mut T → *T`.

**Constructing one**: `<*mut [?]T>some_ptr` / `<*mut T>some_array` — a
plain `Reinterpret` cast in either direction (both sides are already
exactly one leaf, nothing to convert), deliberately **not** requiring the
source pointee to match `T` — the same rule an ordinary `*Foo → *Bar` cast
already follows (every pointer is the same `CastClass` regardless of
pointee). This is what makes the following pattern work, letting a
`marker` stand in for "there's an array starting exactly here" without any
dedicated language feature for it:

```
marker ArrayMarker<T> {
    exposed as_array(*self) => *[?]T {
        <*[?]T>self
    }
}

struct TLVBuffer {
    exposed kind: i32;
    exposed length: u16;
    exposed value: ArrayMarker<u8>;    # zero bytes -- real data starts right here
}
```

`ArrayMarker<T>` is an ordinary `marker` (see [marker
types](20-marker-types.md)) — zero leaves, a real address computed via the
struct's own field layout, `self: *ArrayMarker<T>` reinterpreted to
`*[?]T`. No new `ResolvedType` variant needed for the TLV-tail pattern at
all; it falls out of `marker` plus this cast.

## `[]T`: inferring an array's length from its initializer

The one legal standalone use of `[]T`: a declaration's own type
annotation, paired with an array-literal initializer, infers the real
length from the literal rather than requiring it be written twice:

```
abc : []i8 = [10, 20, 30];   # OK: i8 is inferred from the type,
                              # and the size (3) is inferred from the literal
```

This desugars to an ordinary `[3]i8` declaration — `abc`'s real type is
exactly what writing `[3]i8` by hand would have produced, just without
requiring the caller to count elements and keep that count in sync by
hand. A non-array-literal initializer here (or one whose own element type
doesn't match) is a compile error (`ArraySizeNotInferable`) — there's
nothing to infer a length *from* otherwise.

## `char`, `bool`, and pointer arithmetic

```
if c >= 'A' { if c <= 'Z' { true } else { false } } else { false }

classify(c: char) => *u8 {
    match c {
        'A'..='Z' => <*u8>b"upper\0",
        'a'..='z' => <*u8>b"lower\0",
        '0'..='9' => <*u8>b"digit\0",
    } else { <*u8>b"other\0" }
}
```

`char` supports the full comparison family (`== != < <= > >=`), ordered by
raw codepoint, and can be used as a `match` scrutinee — including range
patterns (`'A'..='Z'`), the same shared range grammar
[ranges elsewhere](05-enums-and-pattern-matching.md) use. `char`'s
`integer_domain()` (what `match` exhaustiveness treats as "the whole
domain") is `0..=0x10FFFF` (`char::MAX`) — the same real range Rust's
`char` occupies. This doesn't carve out the surrogate hole
(`0xD800..=0xDFFF`): an honest pointer reinterpretation can manufacture such
a value, so the continuous interval is a conservative approximation that can
only require extra coverage, never accept an incomplete match.

`char::from_u32` is the supported checked constructor: it rejects values above
`0x10FFFF` and surrogates. It is not an enforcement boundary; pointer casts
are intentionally still able to bypass it. `char` conforms to `Successor` and
skips the surrogate block when iterated, so `U+D7FF` steps to `U+E000` and
`for c in 'a'..='z'` uses the ordinary range protocol.

`char` supports comparisons only. Arithmetic, bitwise operators, and unary
`~` are rejected; cast explicitly (`<u32>c + 1`) when codepoint arithmetic is
actually intended. Pointers still coerce to `usize` for their deliberately
byte-wise arithmetic, but only comparison and pointer subtraction are defined
between two pointers. Pointer-plus/minus-integer operations remain unscaled.

`bool` does **not** belong to that group, despite sharing some operator
spellings: it has no `arithmetic_repr`, so it never decays to an integer and
`true + true` is rejected outright, as are `<`, the shifts, and `~`. Its
operator set is `==`/`!=`, `&`/`|`/`^`, the short-circuiting `&&`/`||`, and
unary `!`. `&`/`|`/`^` evaluate both operands; `&&`/`||` evaluate the right
one only when the left does not already decide the answer. Both forms are
available deliberately — the spelling is what tells a reader whether a call
on the right-hand side runs.
Pointer arithmetic produces `usize`, never a pointer implicitly. `char +
char` and `pointer + pointer` are rejected. A pointer coerces even for
`==`/`!=`, which
is what makes comparing a `*mut T` against a `*T` type-check for free:
both sides become a plain `usize`, so pointee type and mutability never
enter the comparison at all.

`bool` is the one exception that stays **native**, uncoerced: `== != & |
^` all work directly on `bool`, producing `bool`, since `bool` is *closed*
under all five (any combination of `0`/`1` is still `0`/`1`). Arithmetic
and shifts are still not offered on `bool` (`true + true` has no meaning
to fall back on), and neither is unary `~` (bitwise-NOT of `bool`'s `0`/`1`
representation does *not* stay within `{0,1}` the way `& | ^` do). Unary
`!` completes the story and is `bool`-only: it is analysed as `x ^ true`,
which is exactly why it stays inside `{0,1}` where `~` would not.

Casting follows the same asymmetry Rust's own `as` does: `char`/`bool`
both cast *out* to any numeric type freely, but only one direction casts
back *in* — `u8 -> char` (every byte is a valid codepoint) — and nothing
at all casts into `bool` (no implicit "nonzero is true"). Any other
integer into `char` (an arbitrary `u32`, say) is still rejected. Use
`char::from_u32`, the supported validating constructor; pointer reinterprets
remain an intentional bypass in this systems-language model.

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
- ~~**There is no `!` (logical-not) operator at all.**~~ — **added.** `bool`
  now has `!`, `&&` and `||` alongside `&`/`|`/`^`. All three desugar during
  analysis (`!x` to `x ^ true`; `&&`/`||` to the `if`-expressions that were
  previously written by hand), so no `CheckedExpr`, MIR or codegen variant
  was needed for any of them.
- **`isize`/`usize` width is target-dependent by design** — nothing in
  `core` bakes in a `min_value`/`max_value` bound for them, since any
  literal bound would silently be wrong on a target this toolchain wasn't
  built assuming.
