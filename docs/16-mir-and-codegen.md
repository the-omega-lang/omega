# The MIR, and how it reaches Cranelift

## Why this exists

Through `omega-analyzer`, the pipeline is HIR → `CheckedModule`: a fully
resolved, monomorphized tree (see [generics.md](06-generics.md) — a generic
is re-analyzed per concrete instantiation, so by the time a `CheckedModule`
exists there are no type parameters left anywhere in it). Until this stage
was added, `omega-codegen` walked that checked tree directly, doing
control-flow-graph construction and Cranelift instruction emission in the
same recursive walk.

That coupling is fine for exactly one backend. The plan has always been
more than one — Cranelift now, and an LLVM or self-made backend later — and
a second backend built against the checked tree directly would mean
re-solving CFG construction from scratch, in a different crate, using a
different backend's block/value APIs. `omega-mir` is that CFG built once,
as data, with no backend attached to it at all: `omega-codegen` (or any
future backend) consumes it by walking an already-explicit graph of blocks
and terminators, not by re-deriving one from a tree of `if`/`while`/`return`
on the fly.

**Scope note:** this only replaces the *control-flow* half of what
`omega-codegen` used to do itself. Expression evaluation (arithmetic,
calls, casts, aggregates, place projections) is deliberately **not**
flattened to three-address form — see "What's still a tree" below. A
`Vec<Value>`-per-leaf value model (structs/enums/slices as several
positional scalars, not one register or one memory blob) and the rest of
`omega-codegen`'s own architecture are unchanged; a dedicated refactor of
that crate is its own future task.

## Pipeline position

```
HIR → [omega-analyzer] → CheckedModule → [omega-mir] → MirModule → [omega-codegen] → Cranelift IR
```

`omega_mir::lower_program` is the one entry point: every checked module a
compilation produced lowers independently into its `MirModule` counterpart,
one-to-one, in the same order. Nothing here is whole-program-aware —
monomorphization has already fully run by the time a `CheckedModule` exists
(see `omega_driver::compile`), so there's no cross-module state to thread
through a lowering pass that isn't already settled.

## Item level: a mechanical mirror of `CheckedModule`

`MirModule`/`MirItem`/`MirStructDef`/`MirEnumDef`/`MirUnionDef` are a
straight field-for-field copy of their `Checked*` counterparts — a
struct/enum/union/extern/global declaration carries no control flow of its
own, so there's nothing to lower there. `CheckedParam`, `ResolvedType`,
`ResolvedFunctionType`, `ManglingMode`, `InlineMode` are all reused directly
from `omega-analyzer` rather than re-wrapped, the same way `Ident`/`Type`
already cross the HIR/checked-tree boundary unchanged — they're inert data
with no control-flow shape of their own.

Only `MirFunctionDef::body` actually changes shape: a `CheckedBlock` tree
becomes a `MirBody` graph.

## The body: locals and a control-flow graph

```rust
pub struct MirBody {
    pub locals: Vec<MirLocalDecl>,  // 0..arg_count are the parameters, in order
    pub arg_count: usize,
    pub blocks: Vec<MirBlockData>,  // block 0 is always the entry block
}

pub struct MirBlockData {
    pub statements: Vec<MirExprNode>,
    pub terminator: MirTerminator,  // Goto | Branch | Return | Unreachable
}
```

Every `if`/`match`/`while`/`for`/`break`/`continue`/`return`/`defer` in the
checked tree is gone by the time a `MirBody` exists — replaced by an
explicit graph of blocks, each ending in exactly one of four terminators.
This is the actual payoff: `omega-codegen`'s own job shrinks to "for each
`MirBlockData`, translate its statements, translate its one terminator,"
because the graph-construction work already happened once, in
`omega-mir`'s lowering pass, instead of being redone (recursively,
tangled with instruction emission) by every backend.

**Parameters and declared locals share one index space.** `MirBody::locals`
doesn't distinguish a parameter from an ordinary declared variable the way
the checked tree's `Storage::Parameter`/`Storage::Local` tags do — `id <
arg_count` is the only thing that tells them apart, and codegen uses that
one check to decide whether a local's value comes from the entry block's
own Cranelift parameters or a stack slot. A `MirLocalDecl` also covers
lowering-synthesized temporaries with no source declaration of their own
(`source: None`) — today, only a `defer`'s own flag (see below).

## What's still a tree

`MirExpr` mirrors `CheckedExpr` almost exactly (`Place`, `FunctionCall`,
`Assignment`, `BinaryOp`, `Cast`, `StructLiteral`, `EnumConstruct`,
`DynamicCall`, …) — **minus** `If`, `Match`, and `Codeblock`, which are
precisely the variants that become graph shapes instead of expression
nodes. Everything else stays a nested tree, evaluated by
`omega-codegen`'s `process_expr` largely unchanged from before this crate
existed.

This is a deliberate stopping point, not an oversight. A "purer" MIR (and
`rustc`'s own) flattens *everything* to three-address form — every
sub-expression gets its own temporary and its own statement, so a backend
never recurses, only iterates a flat statement list. That buys real things
(local optimizations like CSE become straightforward over a flat
statement stream), but it also means rebuilding `process_expr`'s
arithmetic/call/cast/aggregate/place-projection logic around flat
statements instead of a tree walk — which *is* the dedicated
`omega-codegen` refactor this work was explicitly scoped to not do yet.
Keeping `MirExpr` tree-shaped means that logic ports over with signature
renames only (`Checked*` → `Mir*`), and the door stays open to flattening
it later, inside the CFG shape this crate already establishes, without
another crate-boundary change.

## How control flow lowers

- **`if`/`match` (as a statement or as a value — no distinction needed
  anymore):** each arm's own block ends in a jump to a shared join point.
  Bare `{ }` blocks used as a value lower through the identical machinery,
  treated as a single unconditional arm. A `match` arm's (possibly
  multi-bound) pattern test is a chain of two-way branches, same shape as
  a range pattern's low/high bound; an exhaustive `match` with no `else`
  traps on fall-through, matching the language's own guarantee that this
  point is unreachable.
- **`while`/`for`:** header/body/exit blocks, with `for`'s `continue`
  target being a dedicated block that runs the post-clause before jumping
  back to the header (`continue` still has to run `i++`). `break`/
  `continue` resolve to a direct jump to a block id the lowering pass
  already knows, resolved against its own lexical loop-target stack —
  there is no `HirId`-keyed runtime lookup left anywhere in codegen for
  this.
- **`return`:** never a direct jump out of a nested position. Every
  `return`, and a function body's own implicit tail-expression return,
  routes into one shared exit chain per function — the same "one
  shared exit point" idea the pre-MIR codegen already used, just built as
  real graph structure instead of an `Option<Block>` filled in ad hoc
  during instruction emission.
- **`defer`:** the one genuine unification this crate made possible. A
  `defer`'s flag is just an ordinary synthetic `Bool` local (`source:
  None`) — ordinary in the sense that codegen has no defer-specific
  concept at all: it's a ordinary `Assign` statement setting it `true` at
  the `defer` statement's own position, and an ordinary chain of blocks
  reading it back (FILO, last-declared torn down first) as part of the
  function's exit chain. Before this crate existed, `omega-codegen` had a
  dedicated pre-pass (`collect_defer_ids`) and dedicated per-function
  state (`defer_flags`/`defer_bodies`) just to make this work; none of
  that survived the move — it's exactly the same mechanism every other
  local already uses.

## Avoiding a synthetic local where the value already has a home

The first working version of this crate allocated a fresh temporary local
for *every* `if`/`match` used as a value, unconditionally — correct, but
wasteful: `x := if a { 1 } else { 2 };`, the overwhelmingly common shape,
doesn't need a temporary at all, since the value's real destination (`x`)
is already known before either arm is lowered. The fast path (see
`FunctionLowerer::lower_control_flow_stmt` in `omega-mir`) detects exactly
this — a bare `if`/`match` statement, an assignment to a plain local, or a
`return`/implicit-tail position — and has each arm write straight into the
real destination (or nothing at all, for a bare discarded statement),
skipping both the temporary and the copy. The general (nested-operand)
path is unchanged and still always allocates a temporary, since a sibling
sub-expression lowered afterward could still need to build more blocks
before the value is actually consumed — a real hazard, not a theoretical
one (see Caveats).

## What changed in `omega-codegen`

Deleted outright: `BlockOutcome`, `Codegen::return_block`/`loop_stack`/
`defer_flags`/`defer_bodies`, `LoopTargets`, `collect_defer_ids` and its
three mutually-recursive helpers, and `emit_if`/`emit_match`/`emit_while`/
`emit_for`/`emit_block`/`emit_expr_stmt`/`process_statement`'s
control-flow arms. Replaced by one generic translator in
`define_function_def`: declare a Cranelift `Block` per `MirBlockData` up
front (so a loop's own back-edge always resolves regardless of visit
order), then for each, translate its statements via the largely-unchanged
`process_expr`, then translate its one terminator (`Goto` → `jump`,
`Branch` → `brif`, `Return` → the existing struct-return/plain-return
logic, `Unreachable` → `trap`). Every block is sealed in one pass at the
end, once every terminator (hence every block's predecessor set) is
already known.

`process_decl` disappears as a per-statement operation: since `MirBody::
locals` already lists every local up front, non-parameter locals get their
stack slots allocated lazily, the first time `resolve_place_storage`
actually resolves that local — a branch that never runs never pays for a
slot it never touches, matching what a per-statement-position allocation
already achieved, just triggered by first use instead of by walking a
`Declaration` statement's own lexical position.

## Multiple backends

There are two backends: **Cranelift** (`default`, the fast development
backend) and **LLVM** (the `llvm` feature, via `inkwell`/LLVM 21 — target
breadth, and the features Cranelift lacks). `omgc --backend=<name>` picks
one; `BackendKind::supports(target)` decides whether the chosen one can
serve the chosen target, and a mismatch is one clear error naming both:

```
error: target 'riscv32-unknown-none' is not supported by the 'cranelift'
       backend (supported: x86_64, aarch64)
```

### One decision, one home

The rule the second backend forced: **the shared layers compute every
decided fact exactly once, and a backend reads it rather than re-deriving
it.** Anything re-derived per backend is somewhere the two can silently
disagree — and they must not, because separate compilation means a
Cranelift `core.o` gets linked against an LLVM `main.o` (`just
test-mixed`). Concretely:

| Fact | Home | Backends |
| --- | --- | --- |
| linker symbol, linkage | `MirFunctionDef::symbol`/`linkage` (`omega-mir::mangle`) | read it |
| entry-point `main` | MIR lowering, from `lower_program`'s `entry` | read it |
| sret, parameter flattening, variadic promotion | `omega-codegen::abi` | read it |
| scalar leaf list | `omega_analyzer::layout` (`Leaf`) | map it |
| per-access alignment | `MirPlace::align` | emit it |
| what is not implemented yet | `omega-codegen::preflight` | neither runs |

`preflight` matters more than it looks: with one backend, "unimplemented"
could be a `todo!()` wherever it was noticed. With two, that would make the
*accepted language* depend on `--backend`, so the rejections are shared and
both backends refuse exactly the same programs.

### `Leaf::Ptr` vs `Leaf::Size`

`Leaf` splits the pointer-width scalar in two: `Ptr` is a genuine address,
`Size` a pointer-width *integer* (`usize`/`isize`). Cranelift maps both to
`pointer_type()` and always did — its pointer type *is* an integer type, so
the distinction is invisible there. LLVM cannot: `ptr` and `iN` are
distinct, and typing a `usize` as `ptr` makes arithmetic on it
unrepresentable. This is the shape every such conflation takes — one
backend not needing a distinction is not evidence the shared layer may drop
it.

The LLVM backend holds one invariant throughout: **a value's LLVM type
always matches the leaf list of its MIR type.** `Reinterpret` casts and
pointer arithmetic maintain it with explicit `ptrtoint`/`inttoptr`, and
`bool` is always `i8` (`Leaf::I8`) except at `br`, which is the one
instruction requiring `i1`.

### The verifier

The LLVM backend runs `Module::verify()` before emitting, and turns a
failure into a compile error labelled *"this is a compiler bug, not a
problem with your program"*. LLVM will otherwise write malformed IR — a
type-mismatched `icmp`, a wrong-arity call — straight into an object file
that then crashes at run time with nothing pointing back at the compiler.
Cranelift needs no counterpart: its builder validates as it goes and panics
at the point of construction. This check found real bugs the moment it was
added, which is the argument for it.

### Dispatch

`omega-codegen` doesn't hand `MirModule`s straight to Cranelift-specific
code -- it dispatches through `BackendKind`, an enum with one variant per
Cargo feature the crate enables, each variant gated by its own feature so a
backend nobody compiled in isn't even a choice the type system offers:

```rust
pub fn generate(backend: BackendKind, request: CodegenRequest) -> Result<EmitOutput, String>;
```

`CodegenRequest` bundles everything any backend needs (target, opt level,
emit kind, the mir modules themselves, the entry path, extern functions)
into one named-field struct, replacing what used to be a seven-positional-
argument call. `omgc`'s own `--backend=<name>` flag (`BackendKind::parse`)
is the only place a user ever picks one; `cranelift` is the default, and
`llvm` is available when the crate is built with its feature.

Each backend lives in its own module (both module-private -- nothing
outside the crate ever sees a Cranelift or an LLVM type), and the two are
split by the *same* concerns, file for file, so a change to one has an
obvious counterpart in the other: `mod.rs` (the `Codegen` state struct
itself, `generate`/`finish`), `place.rs` (resolving a `MirPlace` to its
storage), `expr.rs` (evaluating a `MirExprNode`), `function.rs` (building
signatures, declaring/defining a function's body), `item.rs` (the
declare-then-define sweep over every module), `vtable.rs` (spec dynamic-
dispatch vtables), and `leaf.rs` (the one backend-specific seam, below).

**The actual multi-backend enabler** is `crate::layout`, not the
`BackendKind` dispatch itself: struct/enum/union byte-offset/padding/leaf-
count math (`FieldLayout`, `layout_fields`, `type_alignment`,
`enum_*_offset`, ...) used to compute `cranelift::Type`s directly and take
`&Codegen`. It's now backend-agnostic, expressed over a small `Leaf` enum
(`I8`/`I16`/`I32`/`I64`/`F32`/`F64`/`Ptr`/`Size`) and a plain
`pointer_bytes: u32` instead of a Cranelift handle -- `Target::pointer_bytes`
is the one place that width comes from, and `Target` itself lives in
`omega-analyzer` precisely so analysis (`comp` `sizeof`, `usize` range
checking) answers with the *same* width codegen will lay out.
`cranelift::leaf::cranelift_type` and `llvm::leaf::llvm_type` are the only
places a `Leaf` becomes a native type; each backend is that one small
mapping rather than another copy of ~250 lines of layout math. That claim
has now actually been tested by a second backend, and it held.

## Caveats

- **No three-address form yet.** `MirExpr` stays tree-shaped on purpose
  (see "What's still a tree" above); this is the natural next step for
  whenever `omega-codegen` gets its own dedicated refactor, and would open
  the door to real local optimizations (CSE, constant propagation across
  statements) this MIR doesn't attempt today.
- **Block-arguments were tried and rejected as the general mechanism for
  threading an `if`/`match`'s value across its join** — a Cranelift-native
  phi-equivalent, and the more "purely Rust-MIR" choice would be a mutable
  temp local either way (Rust's own MIR has no block-argument mechanism at
  all). The block-argument version broke the moment a *sibling*
  expression built more blocks before the value was actually consumed — a
  real, reproduced bug (a stale value read back from a since-abandoned
  block), not a theoretical one — so every cross-block value in this MIR
  (an `if`/`match` join's result, the function's own return value threaded
  through its `defer` exit chain) is an ordinary local instead, with the
  fast path above recovering the common case's cost back.
- **`MirItem::Declaration`/`MirPlaceRoot::Global` are fully implemented**
  (an ordinary top-level global, `mut` included, with or without a
  compile-time-known initial value — see
  [compile-time-evaluation.md](19-compile-time-evaluation.md)). Extern
  *data* (a non-function `extern`) is the one storage gap left, still
  `todo!()` in `update_extern_decl` — its storage lives in another
  translation unit, a genuinely separate question.
- **Fixed: taking the address of a function parameter directly** (an
  explicit `&param`, or the implicit auto-ref a `*self`/`*mut self` method
  call needs on a by-value parameter, e.g. `key.hash()` where `key: K` is
  a plain parameter) — found and fixed while building `std`'s own
  `HashMap<K, V>`, whose `bucket_index` needs exactly this
  (`key.hash()`). `Codegen::place_storage_address`'s `PlaceStorage::
  Values` arm now lazily spills the parameter's own SSA leaf values into a
  fresh stack slot on demand (the identical lazy-materialization
  `MirPlaceRoot::Expr`/`MirProjection::UnionField` already used elsewhere
  in the same file), sized directly from the leaves' own Cranelift IR
  types rather than needing a `ResolvedType` threaded in.
- **Still `todo!()`: assigning *into* a function parameter directly** (no
  deref in between) — a different code path
  (`Codegen::store_scalars`'s identical `PlaceStorage::Values` arm) than
  the address-of case just above, not fixed by that change. An explicit
  local copy (`mut p := param;`) still works around it today.
