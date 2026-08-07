# Strings, casting & slices

## `*str` vs. `*[u8]` vs. `*u8`

```
s : *str = "hello, world!";
bytes : *[u8] = &[1, 2, 3];
c_string : *u8 = <*u8>b"hello\0";
```

Three genuinely different types that overlap in purpose:

- **`*str`** — a real, nominal, no-null-terminator UTF-8 string type. At
  runtime it's the identical fat-pointer shape `*[u8]` has (`[data ptr, i32
  byte count]`), but there is **zero implicit coercion** to or from
  `Slice`/`Pointer` in either direction — confirmed deliberately, with no
  hidden exception even for literal storage. String literals (`"..."`)
  resolve to `*str`, not `*u8` — a real change from the language's earlier
  design, where they resolved to `Pointer{U8}`. Despite sharing the exact
  same leaf, the two types deliberately spell that second leaf
  differently: `*str`'s is `.size` (a UTF-8 *byte* count — "length" would
  wrongly suggest a character count), `*[T]`'s stays `.length` (a genuine
  element count).
- **`*[u8]`** — an ordinary byte slice, fat pointer, same runtime shape.
- **`*u8`** — a single thin pointer, no length at all. What C string
  functions (`puts`, `printf`'s format string) actually expect.

`str` alone (unwrapped) is never a resolvable type by itself — `*str`/`*mut
str` are resolved via a **raw-syntax peek** (`Type::Named("str")` checked
*before* recursing into ordinary pointee resolution), not the
resolved-*shape*-driven pattern `*[T]`→`Slice` uses. This asymmetry is a
real, load-bearing implementation detail: a `Self`-substituted `str` target
(inside a `for str` spec extension) needed its own explicit fix once this
distinction mattered for something other than a literal syntactic pointer.

## Casting: `<Type>expr`

```
<i64>some_i32
<*u8>some_str            # DropLength: keep the data pointer, drop the length
<*str>some_byte_slice     # Reinterpret: both sides already [ptr, len]
```

Prefix-position, unambiguous (a bare `<` never starts a primary expression
otherwise). Scoped to **numeric types and pointers**. A pointer counts as
an unsigned 64-bit integer for casting purposes — literally true at the
Cranelift IR level (same-width pointers and integers are one Cranelift
type), which is what makes pointer↔pointer and pointer↔integer casts fall
out of the same width/signedness rules with zero special-casing.

- Extend/int-to-float signedness comes from the **source**'s signedness
  (matches Rust's `as`: `-1i8 as u32 == u32::MAX`).
- Float-to-int signedness comes from the **target**'s, and is the
  **saturating** variant (not trapping).
- A pointer cast to a `mut` target requires a `mut` source — the same
  directional rule ordinary pointer coercion already enforces.

Fat-pointer casting (`*str`/`*[u8]`) is genuinely separate machinery from
the scalar cast-kind resolution above (`Slice`/`Str` always return `None`
from the scalar path): fat→fat is `Reinterpret` (leaves already agree),
fat→thin (`*u8`/`*i8`) is `DropLength` (keep the data leaf, drop the length
leaf). There is no reverse (thin→fat) cast — fabricating a length from a
bare pointer isn't something a cast can do.

**Printing a `*str` through C's `printf` soundly** needs `%.*s`, not a bare
`%s` + cast: `printf("%.*s\n", s.size, <*u8>s)` — the byte count consumed
first, matching C's own `%.*s` convention. A bare `%s` on a
non-null-terminated `*str` would read until a stray zero byte.

## Byte strings

```
b"raw bytes, not null-terminated"
```

`b"..."` is `*[u8]` (a slice with a compile-time-known length), never
`*u8`, and never null-terminated — most C-interop call sites need an
explicit trailing `\0` plus a `<*u8>` cast (`<*u8>b"hello\0"`), which is
the overwhelmingly common idiom throughout example code interfacing with
`puts`/`printf`.

## Compile-time slices and fixed arrays

```
&["a", "b", "c"]              # ConstValue::Slice — pointer indirection, rodata blob
[1, 2, 3]                       # (against a SizedArray-typed position) — inline, no indirection
```

`&[...]` is the **only** recognized spelling for a compile-time slice,
everywhere, including inside an enum header field — a bare `[...]` there is
never treated as one (an earlier draft allowed a bare `[...]` specifically
in header position, mirroring how a bare string literal is accepted there;
the user explicitly rejected that as confusable with an ordinary array and
asked for uniform `&[...]`). A bare `[...]` in *ordinary* expression
position still means a stack-allocated `[T; N]` — a different, pre-existing
meaning; `const_eval` tells the two apart by the position's own *expected*
type shape, not by the literal's own syntax.

The difference is real, not just notational: `&[...]` builds a separate,
recursively-constructed static data blob with a pointer relocation into it
(the same relocation mechanism weak-linked data symbols use — see
[modules & linkage](10-modules-and-linkage.md)); a bare `[...; N]` array's
elements live **inline**, with zero indirection, directly in whatever
struct/enum storage contains them.

## Building a fat pointer from scratch: slicing a raw pointer

```
s := &ptr[start..<end]      # => *[T], or *mut [T] if ptr: *mut T
```

Every other way of producing a `*[T]`/`*str` starts from an *existing*
fat pointer or a compile-time-known literal — re-slicing normally needs a
`SizedArray`/`Slice`/`Str` base, and casting is fat→fat or fat→thin only
(never thin→fat, per the caveat above). A raw pointer is *also* a legal
re-slicing base: `&ptr[start..<end]` (or `&ptr[start...end]`) builds a
real `*[T]` directly from `ptr`'s own value (the data pointer) and the
range (the length), which is exactly what an owning, heap-backed
collection (`std::list::List<T>`, `std::string::String`) needs for its
own `as_slice`/`as_str` — see `List<T>::as_slice`'s own implementation.

This is deliberately *not* the same as making a pointer indexable like an
array: plain single-element indexing (`ptr[i]`) on a raw pointer is still
a compile error (`Analyzer::project_index`'s own whitelist never includes
a bare `Pointer`) — only this range-slicing path accepts one, and it
always produces a real, safely indexable `Slice` rather than treating the
pointer itself as array-like. A range sliced off a raw pointer must have
an explicit end (`MissingSliceEnd` otherwise) — unlike `SizedArray`/
`Slice`/`Str`, a bare pointer has no length anywhere to default a missing
one to. Mutability follows the pointer's own: `*mut T` produces `*mut
[T]`, widening to `*[T]` for free at any ordinary coercion site the same
way `*mut T → *T` already does.

`*str` construction needs no separate primitive — `<*str>&ptr[a..<b]`
reuses the already-existing fat→fat `Reinterpret` cast between
`Slice{U8}` and `Str`.

## Unsizing coercion: `*[T; N]` → `*[T]`

```
mut arr : [i32; 4] = [1, 2, 3, 4];
p : *mut [i32; 4] = &mut arr;
s : *[i32] = p;                # implicit -- N (4) becomes the length
```

A pointer to a compile-time-sized array widens implicitly to a slice at
any ordinary coercion site (`let` with a declared type, `return`, a
function argument) — mirroring Rust's own `&[T; N] -> &[T]` unsizing
coercion. Unlike the pointer-mutability widening `accepts` already does
(`*mut T → *T`, purely a relabeling, same one leaf either way), this one
genuinely changes the value's runtime shape: the pointer's own value
stays the data leaf, and a second leaf — `N` itself, always known at
compile time — is synthesized alongside it (`Analyzer::coerce_to_expected`,
`CheckedExpr::UnsizeSlice`). Because of that, it's implemented as a
distinct coercion node rather than folded into `ResolvedType::accepts`
itself: `accepts` is called bare (with no chance to rewrite the value) at
a few sites this coercion doesn't reach yet — struct-literal field
values, array-literal elements, match-arm unification — the same
already-documented gap the `spec *T` dynamic-dispatch coercion has (see
[specs](08-specs.md)). At those sites, a `*[T; N]` written where a
`*[T]` is expected today needs an explicit range slice
(`&arr[0..<N]`/`&arr[0...N-1]`) instead.

Mutability follows the same directional rule as everywhere else: `*mut
[T; N]` widens to either `*[T]` or `*mut [T]`; `*[T; N]` (immutable) only
ever widens to `*[T]`.

## Caveats

- **`*str` is not actually guaranteed valid UTF-8.** The cast family
  treats `Str` and `Slice{U8|I8}` as fully interchangeable in *both*
  directions with no validation — `<*str>some_arbitrary_byte_slice`
  compiles today and freely relabels arbitrary bytes as `*str`, including
  invalid UTF-8. This is a known, explicitly deferred inconsistency (the
  original design intent was an *asymmetric* rule — `*str → *[u8]` free,
  `*[u8] → *str` fallible, mirroring Rust's `str::from_utf8` — but the
  shipped implementation is symmetric). Deliberately deferred by the user
  ("after I implement core, we'll handle that") pending a real
  UTF-8-validating conversion function in `core`; string *literals*
  themselves are still always valid UTF-8 by construction (parsed from a
  UTF-8 source file).
- `const_eval` (compile-time constant evaluation, used for enum header
  fields) does not support casts at all — any header field that would have
  needed a cast (e.g. `*u8` text with a `\0`) had to migrate to `*str`
  instead, since there was no alternative. Body/dynamic fields, struct
  fields, and ordinary expressions have no such restriction.
- `&[...]`/bare-array compile-time construction is deliberately **not
  deduplicated** across occurrences the way string/byte-string data is —
  `ConstValue` isn't cheaply hashable (nests, and floats have no total
  order), and each occurrence is a one-shot construction site, not
  plausibly repeated the way string literals are. A documented
  simplification, not an oversight.
- **Fixed: a closing `>` immediately followed by another closing `>`
  (from a nested generic, a cast, or `sizeof<T>`) used to
  lex as one `>>` token instead of two.** The bug was broader than any
  one construct — nested generics inside an ordinary type position
  (`Bar<Baz<T>>` as a struct field's type) hit it too, not just the cast
  case (`<*mut Node<T>>0`) that surfaced it while writing
  `std::linked_list`/`std::hash_map`; every closing-`>` site in the
  grammar used the same naive single-token check, and nothing anywhere
  actually split a `>>`. Fixed generally rather than per-site:
  `Parser::eat_close_angle`/`expect_close_angle`
  (`compiler/omega-parser/src/parser/mod.rs`) split a `Shr` token in two
  on demand, stashing the leftover `>` in a `pending_gt` slot that
  `peek`/`peek_at`/`advance` all consult ahead of the real token stream —
  so every other parsing function keeps working unmodified once a split
  has happened. All closing-`>` call sites (generic types, generic
  parameters, casts, `sizeof<T>`, and the speculative
  `Optional<T>::Some`-style generic-args parse) now go through these
  instead of a bare `Gt` check. See
  [the standard library](23-standard-library.md).
- **No bare `..` range operator** — only `...` (inclusive) and `..<`
  (exclusive, and its end is mandatory: `a..<` alone is
  `ExclusiveRangeMissingEnd`) exist, matching `match`'s own range-pattern
  grammar exactly (`0...9`, `10..<100`). A cast immediately after `..<`
  is genuinely ambiguous with the operator itself (`0..<i32>len` lexes as
  `0` `..<` `i32` `>` `len`, not `0` `..` `<i32>len`) — bind the cast to a
  local first (`n := <i32>len; &ptr[0..<n]`) rather than writing it
  inline in the range.
