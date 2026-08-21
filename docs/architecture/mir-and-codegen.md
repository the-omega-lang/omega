# MIR and code generation

`omega-mir` is the backend-independent control-flow boundary between the fully checked semantic tree and native emission. `omega-codegen` consumes MIR and emits through LLVM.

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
   v
LLVM
```

## Why MIR exists

The checked tree is excellent for semantic analysis but still represents source control flow recursively. A backend wants explicit blocks and branch/return edges.

Without MIR, codegen would have to rediscover how `if`, `match`, loops, `break`, `continue`, `return`, and `defer` form a CFG directly from the checked tree, entangling control-flow reconstruction with native emission.

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

`MirFunctionDef` also adds two facts computed once, upstream of codegen:

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

`MirFunctionDef.body` is `MirFunctionBody::{Normal(MirBody), Naked(MirInlineAsm)}`. `@naked` functions (see [`../language/functions.md`](../language/functions.md#naked-functions)) never construct a `MirBody`: `lower_naked_body` converts the analyzer-guaranteed single checked `asm` statement directly to `MirInlineAsm`, never invoking `FunctionLowerer` and creating no `LocalId` space, parameter homes, blocks, or defer state for that function. This is structural rather than a boolean guard threaded through the ordinary frame path, so a naked function's "no setup/teardown" invariant cannot be silently weakened by future changes to `MirBody`/`FunctionLowerer`.

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

MIR carries the resolved final type plus an alignment claim so backends do not independently reconstruct place metadata. The current lowering derives that claim with `layout::type_alignment`; projected byte offsets can require weaker effective alignment. The current `@layout(align = n)` address-guarantee limitation remains tracked in `docs/issues/`.

Roots include local/global/expression-derived storage; projections encode operations such as dereference, field, index, and related place transformations.

The analyzer already proved the operation legal and resolved field/type/mutability facts. MIR maps HIR/checked storage identity into `LocalId`/global forms; codegen realizes the memory/SSA mechanics.

## Control-flow lowering

Function lowering is split by responsibility: `lower/function.rs` owns builder state and invariants, `lower/function/control_flow.rs` owns CFG construction, `lower/function/expr.rs` owns ordinary expression translation, and `lower/function/defer.rs` owns the structural defer pre-pass.

### `if` / `match`

Branches become explicit blocks. Value-producing branches write the selected result to a shared local before joining when a value must outlive the branch.

### loops

Loop lowering creates explicit condition/body/post/exit blocks as required. `CheckedBreak`/`CheckedContinue` already carry the target loop's `HirId`; the lowerer maps that ID through its loop-target stack instead of assuming all control flow targets the innermost backend block.

### `return`

Returns route to the function's exit path. If the function has defers, return values are stored before cleanup so all active deferred bodies run before the final `Return` terminator.

### `defer`

A pre-pass allocates a boolean flag for each supported defer. Reaching the source `defer` statement sets its flag; the function exit chain checks flags in reverse order and executes enabled bodies before the final return.

The language restrictions on where defer is currently allowed are semantic-analysis concerns and tracked in language/issues docs; MIR assumes checked input satisfies them.

### `void` and effects

`void` means that an expression produces no value slot; it does not mean the expression can be omitted. MIR still emits a `void`-typed tail or return expression so calls and other effects are preserved.

### `never`

Expressions typed `never` have no usable fallthrough result. MIR emits the expression for its effects and then terminates unreachable continuation rather than inventing a value for later blocks.

## Final symbols and linkage in MIR

MIR lowering translates checked declaration provenance into final object identity before any backend runs.

A function definition therefore reaches codegen with:

```text
symbol: String
linkage: Export | Weak
```

This guarantees codegen never has to decide names/duplicate-folding policy itself.

## Codegen shared layer

`omega-codegen` exposes:

```text
CodegenRequest
OptLevel
EmitKind
EmitOutput
generate(...)
```

`CodegenRequest` contains the translation-unit name, target, MIR modules, entry path, extern-function references, optimization level, and requested output kind. Entry-point identity is consumed while lowering checked modules to MIR; the field remains in the public request shape for compatibility but is not interpreted by LLVM emission.

### Shared preflight

`preflight.rs` rejects constructs that are currently unsupported and must fail before LLVM emission runs. Today that gap is extern data; parameter mutation/addressability is handled by the shared function-storage plan before a body is emitted.

If a new unsupported construct belongs above codegen, put the rejection here or earlier in semantic analysis—not inline in LLVM emission code.

### Target support check

`llvm::supports(target)` is checked before emission. The shared compiler target vocabulary can be broader than what a given LLVM build actually supports.

## Shared ABI and layout

Before native call construction, codegen consumes:

- `omega_analyzer::layout` for leaves/offsets/size/alignment;
- `omega_codegen::abi` for parameter/result calling convention;
- MIR-provided final symbol/linkage.

See [`abi-and-representation.md`](abi-and-representation.md).

## LLVM backend

`src/llvm/` is split by concern:

- `mod.rs` — target machine/triple/data layout setup, shared codegen state, final output;
- `item.rs` — declarations/definitions/globals/externs;
- `function.rs` — function CFG/block emission;
- `expr.rs` — computation/calls/casts/aggregate construction;
- `place.rs` — address/storage/projection loads and stores;
- `leaf.rs` — abstract leaf -> LLVM types;
- `vtable.rs` — dynamic-spec table materialization;
- `inline_asm.rs` — `MirExpr::InlineAsm` lowering: register-class/constraint selection and `$name`/`$N` -> LLVM template-slot binding.

`Codegen` keeps caches for functions, data blobs, globals, vtables, and symbol-collision detection plus per-function local/stack state.

LLVM `alloca` placement is deliberately centralized in the function entry block because allocating in a loop block would allocate on each execution. The backend temporarily repositions its builder to the entry before emitting scratch/local allocations.

The completed LLVM module is always verified before output. A verifier failure is reported as an internal compiler bug because program-invalid inputs should already have been rejected upstream/shared preflight.

### Inline assembly

`MirExpr::InlineAsm` (always `void`-typed, carrying `reg`/`const` operands, `clobber` strings, and the raw template) is the only MIR node whose associated text is never optimized or reinterpreted by Omega. `inline_asm.rs` lowers each `reg` to an early-clobber read-write LLVM constraint (`+&<class>` or `+&{<physical>}`) so the register allocator never assumes an unread value survives past the asm, discards every resulting SSA value, and appends conservative `~{memory}` plus (on X86/X86-64) the `~{dirflag},~{fpsr},~{flags}` status clobbers alongside any user-declared `clobber(...)`. `$name`/`$N` bindings are rewritten directly in the template string: a `reg` becomes the LLVM operand's own `$N` slot (numbered by constraint order, independent of Omega's source binding numbering -- see [`inline-assembly.md`](../language/inline-assembly.md)), and a `const` becomes the analyzer's pre-rendered literal text. `$$` is left untouched because LLVM's own inline-asm template syntax already defines it as one literal `$`. The resulting `inkwell::create_inline_asm` value is always invoked through `build_indirect_call`, matching the LLVM 15+ callable-value requirement for inline asm.

Register-class selection is centralized in `inline_asm.rs` as the one place with target-conditional (`Arch`) codegen logic; every `Arch` Omega currently supports (`X86_64`, `X86`, `Armv7`, `Thumbv7em`, `Aarch64`, `Riscv32`, `Riscv64`) maps each accepted scalar/pointer leaf to a generic LLVM constraint letter. X86/X86-64 asm is always parsed as LLVM's Intel dialect; other targets use their LLVM backend's one defined dialect. Object/assembly emission failure (an integrated-assembler rejection of user-authored instructions/registers) is a `Result` propagated out of `Codegen::finish`, not a panic -- unlike the rest of codegen, invalid inline-asm text is a legitimate user error, not a compiler-bug precondition violation.

### Naked functions

`declare_function_def` attaches LLVM's `naked` and `noinline` enum function attributes whenever `MirFunctionDef.body` is `MirFunctionBody::Naked`, using the same declared `llvm_function_type`/linkage/symbol/section path as an ordinary function -- the caller-facing ABI is unaffected. `define_function_def` matches on `MirFunctionBody` and, for `Naked`, calls a dedicated `define_naked_function` path instead of the ordinary one: it creates exactly one LLVM basic block, calls the existing `process_inline_asm` on the carried `MirInlineAsm`, and emits `build_unreachable`. It never calls `parameter_storage_plan`, reads `function.get_params()` for values, computes a locals layout, or emits an entry alloca/ordinary `Return` terminator -- LLVM's `naked` attribute disables prologue/epilogue emission and forbids IR references to function arguments, so any of that ordinary-path machinery would violate the attribute's contract. `unreachable` is required only because LLVM demands a block terminator; the target asm itself owns real control flow and `unreachable` must never become a machine instruction.

## Declare before define

LLVM emission uses a declare/update pattern so references do not depend on source definition order:

1. declare externally visible/local functions/globals needed by symbols;
2. define function bodies/data after identities exist.

This mirrors the compiler-wide rule that declaration order must not determine semantic availability.

## Codegen-local caches

Codegen may cache native objects keyed by already stable compiler identities, for example:

- `HirId -> native function/global`;
- content hash -> anonymous byte/const blob;
- resolved vtable slot list -> emitted table;
- linker symbol -> declaring ID collision guard.

These caches optimize/organize emission; they must not become new semantic resolution tables.

## Output modes

The same codegen pipeline can produce:

- object bytes;
- textual LLVM IR;
- assembly.

Textual IR is inherently LLVM-specific; object-level external identity/ABI must not be.

## Codegen should not do

Do not place these in codegen:

- overload/name resolution;
- generic inference/instantiation;
- visibility/spec conformance selection;
- source-level control-flow reconstruction;
- independent aggregate field offsets;
- independent linker mangling policy;
- backend-specific acceptance of an otherwise shared language construct.
