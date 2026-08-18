# ABI and implementation representation notes

These implementation notes were separated from the language chapters during the documentation migration. Observable language/ABI promises remain normative under `docs/language/`; this file explains current compiler representation choices.

<!-- migrated from ../language/functions.md -->
## Return-value ABI: hidden struct-return pointer

A return type that flattens to more than the platform's small-value leaf
budget is passed via a hidden struct-return (`sret`) pointer parameter
instead of real return registers — decided once, in `make_function_sig`,
consulted identically by both a function's own definition and every call
site, so the two always agree. This is invisible at the Omega source
level; it only matters when reasoning about generated IR/assembly directly
(`--emit=ir`/`--emit=asm`).

<!-- migrated from ../language/types-and-primitives.md -->
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

<!-- migrated from ../language/compile-time-evaluation.md -->
## Codegen

A `comp`-bound identifier never reaches codegen as a place at all — it's
substituted away during analysis into `CheckedExpr::Const(value)`, exactly
like an ordinary literal, so codegen's job is no larger than "emit a
constant" (already-proven machinery, now extended to structs/enums/unions/
refs, not just numbers/strings/slices). A non-`comp` binding with a `comp`
initializer is an ordinary place whose *initial* bytes happen to already
be known — struct fields are emitted as concatenated leaves (no byte-offset
math needed, mirroring an ordinary struct literal), while enum/union
values are built in an anonymous stack slot at their real byte offsets
(mirroring ordinary `EnumConstruct`/`UnionConstruct` codegen) and read
back out as leaves.

Every anonymous rodata blob a `ConstValue` (or a `&[...]` compile-time
slice) gets emitted into is content-addressed and deduplicated by
`Codegen::const_blobs`, keyed by the same content hash used to name the
blob's own symbol — needed once const promotion (above) could reasonably
emit *the same* comp value's content from two independent call sites
(e.g. `&SIZE` and a `*self`-receiver method call on `SIZE`, both in the
same compile): without a dedup check before `define_data`, the second
emission would try to define an already-defined symbol and the linker
step would reject it.
