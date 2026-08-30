# Omega Architecture

This file is the **compact implementation map** for contributors and coding agents. It answers where a behavior lives and which deeper architecture document to open. It is not the Omega language definition and should not grow into a second architecture manual.

For exact language semantics, use [`docs/language/`](docs/language/). For deeper compiler/runtime mechanics, use [`docs/architecture/`](docs/architecture/). For writing Omega code, start with [`docs/guide/quick-reference.md`](docs/guide/quick-reference.md).

## Navigation rules

1. Identify the owning subsystem here.
2. Open at most the relevant deep architecture document(s) from [`docs/architecture/README.md`](docs/architecture/README.md).
3. Search for the concrete symbols involved before opening large source files.
4. Treat crate boundaries as context boundaries; cross them only when the contract actually changes.
5. Do not read all callers/callees or historical plans “for completeness”.
6. Stop once ownership, affected interfaces, invariants, and verification boundaries are clear.

`docs/issues/` records known deviations/debt. `docs/plan/` is historical cold storage.

## Two pipeline views

Omega has a **control/orchestration flow** and a **representation/data flow**. They answer different questions.

### Control / orchestration

Compilation is initiated by the CLI and semantically orchestrated by the driver:

```text
omgc
  |
  | constructs Driver
  v
omega-driver
  |  package/module discovery
  |  parse + HIR loading
  |  per-item semantic queries
  v
CompiledProgram
  |
  | omega_mir::lower_program
  v
MIR modules
  |
  | omega_codegen::generate
  v
object / LLVM IR / assembly
```

`omega-driver::Driver::compile` is the main semantic-compilation entry point. `omgc` owns the outer toolchain handoff from semantic compilation to MIR/codegen and output files.

### Representation / data

```text
source text
   |
   v
omega-parser       tokens -> AST -> macro-expanded AST
   |
   v
omega-hir          stable post-expansion HIR
   |
   v
omega-analyzer     checked tree + resolved semantic types
   |                ^
   |                | cross-module/query lifetime owned by omega-driver
   v
omega-mir          explicit CFG + tree-shaped computations
   |
   v
omega-codegen      LLVM
   |
   v
native output
```

The parser is the first source transformation stage, not the whole-compilation orchestrator.

Deep overview: [`docs/architecture/compiler-overview.md`](docs/architecture/compiler-overview.md).

## Ownership map

| Area | Primary owner | Deep architecture doc |
|---|---|---|
| spans, structured diagnostics, rendering | `omega-diagnostics` | [`diagnostics.md`](docs/architecture/diagnostics.md) |
| lexing, grammar, AST, macros/reparse | `omega-parser` | [`parsing-and-hir.md`](docs/architecture/parsing-and-hir.md) |
| post-expansion identity and syntax-only desugaring | `omega-hir` | [`parsing-and-hir.md`](docs/architecture/parsing-and-hir.md) |
| semantic rules, checked representation | `omega-analyzer` | [`semantic-analysis.md`](docs/architecture/semantic-analysis.md) |
| semantic type graph, layout, const-eval, shared target vocabulary | `omega-analyzer` | [`types-layout-and-const-eval.md`](docs/architecture/types-layout-and-const-eval.md) |
| package/module discovery, query caches, cross-module resolver | `omega-driver` | [`module-driver-and-linkage.md`](docs/architecture/module-driver-and-linkage.md) |
| CFG lowering and final linker symbol/linkage assignment | `omega-mir` | [`mir-and-codegen.md`](docs/architecture/mir-and-codegen.md), [`symbol-mangling.md`](docs/architecture/symbol-mangling.md) |
| shared Omega calling convention | `omega-codegen::abi` | [`abi-and-representation.md`](docs/architecture/abi-and-representation.md) |
| native emission | `omega-codegen` | [`mir-and-codegen.md`](docs/architecture/mir-and-codegen.md) |
| symbol grammar/encoding/decoding | `omega-mangle` | [`symbol-mangling.md`](docs/architecture/symbol-mangling.md) |
| CLI/output/toolchain orchestration | `omgc` | [`compiler-overview.md`](docs/architecture/compiler-overview.md) |
| portable/runtime library layers | `runtime/` | [`runtime-and-platform.md`](docs/architecture/runtime-and-platform.md) |
| architectural verification boundaries | crate tests + root `tests/` + `bin/test-runner` + `justfile` | [`testing-and-validation.md`](docs/architecture/testing-and-validation.md) |

## Compiler crates

### `compiler/omega-diagnostics`

Language-agnostic diagnostic substrate: spans, source positions, structured findings, labels/footers, rendering, and the highlighting interface.

**Boundary:** feature crates decide *what is wrong*; this crate decides how structured diagnostics are represented/rendered.

### `compiler/omega-parser`

Owns source text through macro-expanded AST:

```text
text -> lexer -> tokens -> recursive parser -> AST -> macro expansion/reparse -> AST
```

Important areas: `lexer.rs`, `ast/`, `parser/cursor.rs`, `parser/item/`, `parser/expression.rs`, `parser/statement.rs`, `macros.rs`, `macros/expander.rs`, `diagnostics.rs`, `prelude.rs`.

**Boundary:** syntax only. Semantic validity requiring names/types belongs downstream.

### `compiler/omega-hir`

Owns the first stable post-macro-expansion representation and `HirId` identity. `lower.rs` is the lowering entry point; `lower/item.rs`, `lower/statement.rs`, and `lower/expression.rs` perform infallible syntax-only normalization/desugaring.

**Boundary:** no name resolution, type checking, or backend behavior.

### `compiler/omega-analyzer`

Owns semantic analysis. A short-lived `analysis::Analyzer` checks a focused top-level signature/body and obtains cross-module facts through `ModuleResolver`.

Important areas: `analysis/items/`, `analysis/stmts.rs`, `analysis/places/`, `analysis/paths.rs`, `analysis/calls/`, `analysis/exprs/`, `analysis/specs.rs`, `checked.rs`, `resolved_type.rs`, `resolver.rs`, `generics.rs`, `comp_eval.rs`, `layout.rs`, `target.rs`, `error/`. Item signature work and body checking are separated inside `analysis/items/`; place roots, field/index projection, and slicing are separated inside `analysis/places/`.

**Boundary:** semantic algorithms live here; filesystem/module/query lifetime does not.

### `compiler/omega-driver`

Owns long-lived semantic-compilation state and cross-module orchestration:

- package/module discovery and source inventory;
- parsed AST/HIR/module-index caches;
- imports and resolver state;
- memoized item/spec/overload queries;
- primitive/conformance registrations;
- generic-instantiation discovery/materialization;
- accumulation of diagnostics across short-lived analyzers.

The core named-item query identity is conceptually `(module, name, generic_args)` -- where a generic argument is a resolved type or a canonical compile-time value -- allowing local, external, and concrete generic items to use the same demand-driven machinery. Item/spec queries have explicit `InProgress / Resolved / Failed` states, and ordered resolution stacks preserve dependency chains for cycle diagnostics. Primitive registration/materialization lives in `primitives.rs`; conformance registration and goal solving are separated under `conformances/`. Compilation orchestration is split under `compile/` into signature collection, body materialization, and final output sweeps.

**Boundary:** the driver orchestrates/owns lifetime; semantic rules remain in the analyzer.

### `compiler/omega-mir`

Lowers checked functions into backend-independent basic-block CFGs while keeping ordinary computations tree-shaped. Parameters and locals share a `LocalId` space; terminators make control transfer explicit.

MIR lowering also assigns final linker symbols and strong/weak linkage, so codegen consumes fully decided linkage rather than deriving it.

### `compiler/omega-codegen`

Consumes MIR through a shared `CodegenRequest`. Shared preflight, layout/ABI inputs, symbols, and linkage must agree before LLVM emission (`llvm/`).

**Boundary:** LLVM translates already-decided semantics. It should not independently implement overload resolution, language validity, aggregate layout policy, or symbol identity.

### `compiler/omega-mangle`

Standalone symbol encoder/decoder/demangler. It intentionally does not depend on analyzer/compiler representations; `omega-mir` adapts compiler identities/types to the mangling vocabulary.

### `compiler/omgc`

CLI/toolchain frontend. Parses options, constructs the driver, calls semantic compilation, lowers the returned program to MIR, invokes codegen, and writes output.

## Cross-cutting owners

Some concepts span multiple phases but must still have one decision owner:

- **Grammar/source shape:** parser.
- **Stable source identity:** HIR.
- **Language semantics:** analyzer; driver supplies cross-module/query state.
- **Aggregate size/alignment/field offsets/leaves:** `omega-analyzer::layout`.
- **Compile-time semantic values:** analyzer/driver const-eval query path.
- **Shared Omega parameter/result ABI:** `omega-codegen::abi`.
- **Mangled symbol grammar:** `omega-mangle`.
- **Concrete final symbol + strong/weak linkage:** MIR lowering.
- **Backend-native values/blocks/objects:** `omega-codegen::llvm`.
- **Source diagnostic rendering:** `omega-diagnostics` + CLI source lookup/highlighting.

Later phases should consume these decisions rather than re-derive them.

## Runtime and packages

The runtime is separate from the compiler workspace and is compiled as ordinary Omega packages/objects:

```text
runtime/core       portable foundational package; limited ambient compiler privilege
runtime/std        higher-level standard library
runtime/plat/*     platform capability implementations presented as package `plat`
runtime/shims      target-specific low-level assembly glue
```

`core` owns primitives/inherent methods and ambient exposed names/macros. `std` and `plat` are ordinary explicitly registered packages. Platform capability composition uses Omega `gap`/`glue`, not an implicit native runtime registry.

## Testing and validation

Omega separates compiler-component testing from language conformance:

- crate-local Rust tests verify parser/analyzer/driver/MIR/codegen/mangling behavior at the narrowest useful boundary;
- root `tests/<case>/` directories are real Omega packages that exercise observable language behavior through the actual compiler, linker, runtime, and executable;
- `bin/test-runner` discovers those root cases, compiles them with the registered runtime packages, links against the prebuilt runtime objects, executes successful compilations, and compares optional `expected.stdout` / `expected.stderr` files exactly;
- `just test-all` is the normal top-level gate that prepares the required artifacts before invoking the runner; direct `bin/test-runner` invocation is the focused path when artifacts are already built.

The language suite is conformance evidence for [`docs/language/`](docs/language/): tests should encode what the specification promises, not normalize accidental compiler behavior. See [`testing-and-validation.md`](docs/architecture/testing-and-validation.md) for test selection, negative-test rules, separate-compilation coverage, and artifact conventions.

## Architectural invariants

When changing the compiler, preserve these unless the change intentionally redesigns them:

1. **One fact, one owner.** Do not duplicate semantic/layout/ABI/symbol decisions across phases.
2. **Resolve once, read back later.** Store decided facts and consume them downstream.
3. **Shared decisions stay shared.** Validity, layout, ABI, symbols, and linkage are decided once upstream of LLVM emission and must not be re-derived or duplicated inside `omega-codegen`.
4. **Crate boundaries are contracts.** Cross them through public data/interfaces rather than duplicating another crate's logic.
5. **Determinism is observable.** IDs, diagnostics, declarations, symbols, and output order must not accidentally depend on randomized map/set iteration.
6. **Separate compilation is real.** Package identity, mangling, ABI, and duplicate weak monomorphization behavior are cross-process contracts.
7. **Semantic invalidity belongs before backend emission** wherever it can be rejected uniformly.

## Task routing

### Syntax/grammar only

Start with `omega-parser` + relevant `docs/language/` grammar/feature chapter. Open HIR only if the downstream source shape changes.

### Syntax desugaring / stable identity

Start with `omega-hir` + [`parsing-and-hir.md`](docs/architecture/parsing-and-hir.md). Open analyzer only if semantics change.

### Names, types, calls, generics, specs, patterns, compile-time semantics

Start with `omega-analyzer` + the relevant normative language chapter. Open driver only for cross-module/query/cache/instantiation-lifetime questions.

### Packages, imports, externs, demand-driven item resolution

Start with `omega-driver` + [`module-driver-and-linkage.md`](docs/architecture/module-driver-and-linkage.md).

### Runtime control flow

Start with `omega-mir` + [`mir-and-codegen.md`](docs/architecture/mir-and-codegen.md).

### Representation / ABI change

Start with semantic type + [`types-layout-and-const-eval.md`](docs/architecture/types-layout-and-const-eval.md) and [`abi-and-representation.md`](docs/architecture/abi-and-representation.md). Audit MIR, `omega-codegen::llvm`, mangling, and separate-compilation tests only if their contracts are affected.

### Backend-only emission issue

Start with `omega-codegen::llvm`.

### Symbols / linkage / duplicate monomorphizations

Start with [`symbol-mangling.md`](docs/architecture/symbol-mangling.md), then `omega-mir` and `omega-mangle`; include driver only when ownership/provenance changes.

### Runtime/core/std/platform capability

Start with the relevant `runtime/` package + [`runtime-and-platform.md`](docs/architecture/runtime-and-platform.md). Keep compiler internals closed unless compiler privilege/language support changes.

## Documentation authority

- `docs/language/` — normative intended Omega semantics.
- `docs/architecture/` — intended compiler/runtime architecture and ownership.
- current source — exact mechanics the compiler currently implements.
- `docs/guide/` — explanatory/programmer-facing material.
- `docs/issues/` — known deviations, unsupported cases, and design debt.
- `docs/plan/` — historical intent; not current authority.

If implementation behavior conflicts with `docs/language/`, check `docs/issues/` rather than silently redefining the language. If source mechanics conflict with architecture docs, establish whether the source or architecture documentation is stale before propagating the discrepancy.
