# MIR and code generation

`omega-mir` is the backend-independent control-flow boundary between the fully checked semantic tree and native emission. `omega-codegen` consumes MIR through either Cranelift or LLVM.

```text
CheckedModule
   |
   | omega_mir::lower_program
   v
MirModule / MirBody CFG
   |
   | omega_codegen::generate
   v
shared preflight + ABI
   |
   +--> Cranelift
   |
   +--> LLVM
```

## Why MIR exists

The checked tree is excellent for semantic analysis but still represents source control flow recursively. A backend wants explicit blocks and branch/return edges.

Without MIR, each backend would independently have to rediscover how `if`, `match`, loops, `break`, `continue`, `return`, and `defer` form a CFG. That would duplicate semantics at the worst possible boundary.

MIR builds the CFG once.

## MIR scope

MIR is intentionally **not** a full three-address SSA IR.

It lowers control flow, while ordinary computation remains tree-shaped:

- arithmetic/logical operations;
- calls;
- casts;
- aggregate construction;
- places/projections;
- constants;
- dynamic calls.

This keeps MIR's current responsibility narrow: backend-independent control-flow structure + final emission facts.

A future expression-normalization/optimization IR can be added deliberately; it should not emerge accidentally as one backend grows local rewrites.

## Program lowering

`omega_mir::lower_program` is the crate entry point.

It receives the checked modules and package entry path and returns corresponding `MirModule`s. Semantic monomorphization is already complete; lowering does not resolve new cross-module semantic facts.

Modules lower independently.

## Item-level MIR

Most item shapes mechanically mirror checked items:

- global declaration;
- extern declaration;
- struct/union/enum definition;
- function definition.

Only function bodies fundamentally change shape.

`MirFunctionDef` also adds two facts computed once for all backends:

- final linker `symbol`;
- `MirLinkage`.

See [`symbol-mangling.md`](symbol-mangling.md).

## Function-body CFG

```rust
MirBody {
    locals: Vec<MirLocalDecl>,
    arg_count: usize,
    blocks: Vec<MirBlockData>,
}

MirBlockData {
    statements: Vec<MirExprNode>,
    terminator: MirTerminator,
}
```

Block 0 is the entry block. Every block ends in exactly one terminator:

```text
Goto
Branch
Return
Unreachable
```

An unterminated final builder block is a MIR-lowering bug.

## Unified locals

Function parameters and locals share one `LocalId` space:

```text
0 .. arg_count                 parameters
arg_count .. locals.len()      declared or synthesized locals
```

A `MirLocalDecl.source` is the originating `HirId` when one exists; synthesized temporaries have no source ID.

Codegen uses `id < arg_count` to distinguish parameter-entry storage from ordinary local-frame storage.

## Cross-block values

MIR does not use block arguments as its general value-merge mechanism. Values that must survive across control-flow joins are stored in ordinary synthetic locals.

Examples include:

- `if`/`match` expression results;
- function return value while routing through `defer` cleanup.

This is deliberately backend-neutral and avoids coupling MIR to one backend's block-parameter model.

Fast paths can avoid a temporary where an expression is immediately assigned/returned and no sibling expression needs the value later.

## Place representation

`MirPlace` is a resolved storage path:

```text
root + typed projections + final type/alignment
```

The final type and base alignment are carried from checked lowering so backends do not re-derive them independently. The alignment is a property of the place base; projected byte offsets can require weaker effective alignment. The current `@layout(align = n)` address-guarantee limitation remains tracked in `docs/issues/`.

Roots include local/global/expression-derived storage; projections encode operations such as dereference, field, index, and related place transformations.

The analyzer already proved the operation legal and resolved field/type/mutability facts. MIR maps HIR/checked storage identity into `LocalId`/global forms; codegen realizes the memory/SSA mechanics.

## Control-flow lowering

`lower/function.rs` owns function CFG construction.

### `if` / `match`

Branches become explicit blocks. Value-producing branches write the selected result to a shared local before joining when a value must outlive the branch.

### loops

Loop lowering creates explicit condition/body/post/exit blocks as required. `CheckedBreak`/`CheckedContinue` already carry the target loop's `HirId`; the lowerer maps that ID through its loop-target stack instead of assuming all control flow targets the innermost backend block.

### `return`

Returns route to the function's exit path. If the function has defers, return values are stored before cleanup so all active deferred bodies run before the final `Return` terminator.

### `defer`

A pre-pass allocates a boolean flag for each supported defer. Reaching the source `defer` statement sets its flag; the function exit chain checks flags in reverse order and executes enabled bodies before the final return.

The language restrictions on where defer is currently allowed are semantic-analysis concerns and tracked in language/issues docs; MIR assumes checked input satisfies them.

### `never`

Expressions typed `never` have no usable fallthrough result. MIR terminates unreachable continuation appropriately rather than inventing a value for later blocks.

## Final symbols and linkage in MIR

MIR lowering translates checked declaration provenance into final object identity before any backend runs.

A function definition therefore reaches codegen with:

```text
symbol: String
linkage: Export | Weak
```

This guarantees Cranelift and LLVM cannot disagree about names/duplicate-folding policy.

## Codegen shared layer

`omega-codegen` exposes:

```text
BackendKind
CodegenRequest
OptLevel
EmitKind
EmitOutput
generate(...)
```

`CodegenRequest` contains the target, modules, entry path, extern-function references, optimization level, and requested output kind.

### Shared preflight

`preflight.rs` rejects constructs that are currently unsupported and must fail identically regardless of backend (for example current parameter-assignment / extern-data gaps).

If a new unsupported construct is common to all backends, put the rejection here or earlier in semantic analysis—not in two backend-specific `todo!()` branches.

### Backend support check

`BackendKind::supports(target)` is checked before emission. The shared compiler target vocabulary can be broader than one backend's supported set.

## Shared ABI and layout

Before backend-native call construction, both backends consume:

- `omega_analyzer::layout` for leaves/offsets/size/alignment;
- `omega_codegen::abi` for parameter/result calling convention;
- MIR-provided final symbol/linkage.

See [`abi-and-representation.md`](abi-and-representation.md).

## Cranelift backend

`src/cranelift/` is split by concern:

- `mod.rs` — ISA/module setup, shared backend state, final output;
- `item.rs` — declarations/definitions/globals/externs;
- `function.rs` — function CFG/block emission;
- `expr.rs` — computation/calls/casts/aggregate construction;
- `place.rs` — address/storage/projection loads and stores;
- `leaf.rs` — abstract leaf -> Cranelift types;
- `vtable.rs` — dynamic-spec table materialization.

`Codegen` keeps caches for functions, data blobs, globals, vtables, and symbol-collision detection plus per-function local/stack state.

Cranelift validates block/function structure while building; target ISA/triple and optimization settings remain backend-local.

Cranelift does not expose a native variadic-function call shape suitable for Omega's current path, so a variadic call is emitted with a fixed signature synthesized for that concrete call site after shared C default-argument promotion. This is a backend translation detail; promotion policy remains in the shared ABI layer.

## LLVM backend

`src/llvm/` intentionally mirrors the same conceptual split:

- `mod.rs`;
- `item.rs`;
- `function.rs`;
- `expr.rs`;
- `place.rs`;
- `leaf.rs`;
- `vtable.rs`.

LLVM-specific target machine/triple/data layout/optimization configuration stays in this backend.

LLVM `alloca` placement is deliberately centralized in the function entry block because allocating in a loop block would allocate on each execution. The backend temporarily repositions its builder to the entry before emitting scratch/local allocations.

The completed LLVM module is always verified before output. A verifier failure is reported as an internal compiler bug because program-invalid inputs should already have been rejected upstream/shared preflight.

## Declare before define

Both backends use a declare/update pattern so references do not depend on source definition order:

1. declare externally visible/local functions/globals needed by symbols;
2. define function bodies/data after identities exist.

This mirrors the compiler-wide rule that declaration order must not determine semantic availability.

## Backend-local caches

Backends may cache native objects keyed by already stable compiler identities, for example:

- `HirId -> native function/global`;
- content hash -> anonymous byte/const blob;
- resolved vtable slot list -> emitted table;
- linker symbol -> declaring ID collision guard.

These caches optimize/organize emission; they must not become new semantic resolution tables.

## Output modes

The same backend pipeline can produce:

- object bytes;
- textual backend IR;
- assembly.

Textual IR is inherently backend-specific; object-level external identity/ABI must not be.

## Backend parity rule

When a change affects a shared contract, inspect/test both backends. When a bug is truly backend-local, do not read/edit the other backend merely for symmetry.

Shared-contract examples:

- new MIR variant;
- new `ResolvedType` representation;
- layout/ABI change;
- vtable shape;
- constant representation;
- final symbol/linkage change;
- new target-width behavior.

## Codegen should not do

Do not place these in backend code:

- overload/name resolution;
- generic inference/instantiation;
- visibility/spec conformance selection;
- source-level control-flow reconstruction;
- independent aggregate field offsets;
- independent linker mangling policy;
- backend-specific acceptance of an otherwise shared language construct.
