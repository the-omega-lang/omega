# Omega Architecture

This file is a **navigation map for contributors and coding agents**. It is intentionally compact. It does not replace the source code or the topic documentation under `docs/`.

Use it to answer two questions before exploring:

1. **Which subsystem owns this behavior?**
2. **What is the smallest set of docs and source files needed for this task?**

## Agent navigation rules

- Start here, then read only the relevant topic docs, then search the source for concrete symbols.
- Do **not** follow `docs/README.md`'s full reading order for an ordinary task; that sequence is for human onboarding.
- Prefer symbol/reference search and targeted source reads over reading large files in full.
- A crate boundary is a default context boundary. Cross it only when a concrete dependency requires it.
- Do not recursively inspect callers, callees, neighboring modules, tests, or backends "for completeness".
- Stop exploring once the behavior, affected interfaces, invariants, and verification path are clear enough to act safely.
- `docs/` describes the current design. `docs/plan/` is historical planning material and should be treated as **cold storage**; inspect it only when current docs/source do not explain necessary rationale.
- Source code is authoritative for current implementation details. Current docs are authoritative for intended/current design and known caveats.

---

## Repository map

There are two useful views of the compiler. Do not confuse the **control/orchestration flow** with the **representation/data flow**.

### Control / orchestration flow

`omgc` is the CLI entry point. It constructs `omega_driver::Driver` and calls `Driver::compile`. The driver owns whole-program discovery and semantic-compilation orchestration: it discovers modules, asks the parser/HIR pipeline to load them, and drives analyzer queries. After `Driver::compile` returns a `CompiledProgram`, `omgc` lowers the checked modules to MIR and invokes code generation.

```text
omgc
  |
  v
omega-driver          package/module discovery + whole semantic compilation
  |
  +---- invokes ------> omega-parser -> omega-hir
  |                          |
  +---- drives --------> omega-analyzer
  |
  v
CompiledProgram
  |
  v
omgc -> omega-mir -> omega-codegen -> object / IR / asm
```

### Representation / data flow

If the question is instead "what representation does source pass through?", the pipeline is:

```text
source text
    |
    v
omega-parser      tokens, AST, macro expansion
    |
    v
omega-hir         stable post-expansion HIR
    |
    v
omega-analyzer    checked/typed representation
    |
    v
omega-mir         backend-independent control-flow representation
    |
    v
omega-codegen     Cranelift / LLVM
    |
    v
object / IR / asm
```

The parser is therefore the first **source transformation stage**, but it is not the top-level compilation orchestrator. For whole-build control flow, start at `omgc` / `omega-driver`.

Supporting crates and trees:

```text
omega-diagnostics   spans, structured diagnostics, rendering
omega-mangle        standalone Omega linker-symbol encoding/decoding
omgc                CLI frontend that connects driver -> MIR -> codegen
runtime/core        portable core language/runtime facilities
runtime/std         standard library
runtime/plat        default platform glue
runtime/shims       target-specific low-level assembly glue
examples/           end-to-end example programs
```

---

# Compiler crates

## `compiler/omega-diagnostics`

**Owns:** the compiler-wide diagnostic foundation.

- `span.rs` — source spans.
- `source.rs` — source-file position translation.
- `diagnostic.rs` — structured findings, labels, severity, footers.
- `render.rs` — Rust-style terminal rendering.
- `highlight.rs` — syntax-highlighting interface used by renderers.

**Boundary:** this crate deliberately knows nothing about Omega syntax, types, modules, or semantic analysis. Compiler stages convert their own structured errors into diagnostics; rendering happens above them.

**Look here when:** changing diagnostic infrastructure, span representation, rendering, or highlighting contracts shared by multiple stages.

---

## `compiler/omega-parser`

**Owns:** source text through macro-expanded AST.

Pipeline inside the crate:

```text
text -> lexer -> tokens -> parser -> AST -> macro expansion/reparse -> AST
```

Key areas:

- `lexer.rs` — tokenization.
- `ast/` — parser AST definitions.
- `parser/` — recursive-descent parser.
  - `expression.rs` — expression grammar.
  - `item.rs` — top-level/item grammar.
  - `statement.rs` — statement grammar.
  - `type.rs` — type grammar.
  - `contextual.rs` — contextual-keyword registry/recognition.
  - `recovery.rs` — parser recovery.
  - `macro_syntax.rs` — macro syntax parsing.
- `macros.rs` — token substitution and re-parsing for macro expansion.
- `diagnostics.rs` — lexer/parser diagnostics.
- `prelude.rs` — supported public surface consumed by other crates.
- `lib.rs` — `SourceModule::parse` entry point.

**Important invariants:**

- Macro expansion reparses tokens, so identities assigned before expansion are not stable.
- Contextual keywords should commit only when the surrounding grammar shape confirms the keyword use.
- The parser handles syntax, not semantic validity that requires name/type information.

**Primary docs:**

- `docs/15-parsing-and-hir.md`
- `docs/12-macros.md`
- Topic-specific language docs for the syntax being changed.

**Default context wall:** do not enter analyzer/MIR/codegen for a parser-only grammar change unless downstream representation or semantics actually change.

---

## `compiler/omega-hir`

**Owns:** the first stable post-macro-expansion representation.

Key files:

- `hir.rs` — HIR node definitions.
- `ids.rs` — `HirId`, `ModuleId`, stable identities used downstream.
- `lower.rs` — AST -> HIR lowering.
- `lib.rs` — public surface and architectural rationale.

HIR lowering is intentionally **infallible** and performs syntax-only normalization/desugaring. It currently owns structural transformations such as synthetic `self` handling, `spec T` parameter lowering, and flattened place chains.

**Does not own:** name resolution, type checking, semantic validation, or backend concerns.

**Primary doc:** `docs/15-parsing-and-hir.md`.

**Look here when:** a syntax change must survive macro expansion with stable identity, or when a syntax-only desugaring should be represented consistently for all later phases.

---

## `compiler/omega-analyzer`

**Owns:** semantic analysis: HIR in, fully typed/checked representation out.

One `analysis::Analyzer` checks one top-level signature or body and is then discarded. Cross-module knowledge is obtained through `resolver::ModuleResolver`; the analyzer itself does not own filesystem/module caches.

Major areas:

- `analysis/`
  - `items.rs` — item/signature analysis.
  - `exprs.rs` — expression analysis.
  - `calls.rs` — function/method call resolution.
  - `specs.rs` — spec/conformance behavior.
  - `paths.rs` — semantic path resolution.
  - `patterns.rs` — pattern analysis.
  - `places.rs` — place/addressability semantics.
  - `stmts.rs` — statement analysis.
  - `consts.rs`, `literals.rs`, `visibility.rs` — focused semantic areas.
- `checked.rs` — checked tree consumed by later phases.
- `resolved_type.rs` — semantic type representation.
- `resolver.rs` — resolver interface the driver implements.
- `context.rs` — analyzer-local/context machinery.
- `generics.rs` — generic analysis/instantiation support.
- `comp_eval.rs` — compile-time evaluation.
- `layout.rs` — struct/enum/union byte layout.
- `exhaustiveness.rs` — pattern exhaustiveness.
- `annotations.rs` — semantic annotation handling.
- `dead_code.rs` — dead-code analysis.
- `target.rs` — backend-independent target vocabulary.
- `error/` — structured analysis errors and rendering conversion.

**Important invariants:**

- Nodes are identified by the explicit `(HirId, Span)` pair.
- Resolve facts once at signature time and read them back later rather than re-deriving them.
- Generic instantiations are re-analyzed/monomorphized rather than type-erased.
- Module/filesystem ownership belongs to the driver, not the analyzer.

**Primary docs:** topic docs under `docs/`, especially:

- `docs/06-generics.md`
- `docs/07-visibility.md`
- `docs/08-specs.md`
- `docs/09-annotations.md`
- `docs/19-compile-time-evaluation.md`
- `docs/17-design-review.md`

**Default context wall:** for semantic work, begin inside `omega-analyzer`, the HIR types it consumes, and the relevant design docs. Parser, MIR, and codegen remain closed unless the semantic change affects their contracts.

---

## `compiler/omega-driver`

**Owns:** whole-compilation orchestration above the per-item analyzer. `omgc` constructs `Driver` and calls `Driver::compile`; this is the main entry point for the semantic compilation of a package.

Core architectural rule: **every top-level item is an independent memoized query**. Same-module references, cross-module references, and generic instantiations all go through the same query model.

Key files, roughly in dependency/order-of-work order:

- `roots.rs` / `fs_resolve.rs` — package roots and filesystem lookup.
- `modules.rs` — parsing, module indexing, import-graph walking.
- `diagnostics.rs` — collection of findings and analyzer invocation.
- `items.rs` — phase-1 item/signature queries.
- `bodies.rs` — phase-2 body checking using signature results.
- `conformances.rs` — project-level conformance collection/handling.
- `compile.rs` — whole-program two-phase sweep.
- `resolver.rs` — implementation of the analyzer's `ModuleResolver` interface.
- `error.rs` — `CompileError` and final `CompiledProgram`.

`CompiledProgram` contains checked modules, the entry module, warnings, and referenced extern functions. It is the semantic output consumed by the CLI before MIR lowering.

**Primary docs:**

- `docs/10-modules-and-linkage.md`
- `docs/06-generics.md`
- Relevant feature docs when changes affect cross-module semantics.

**Look here when:** changing module discovery/imports, package roots, declaration order/query behavior, cross-module resolution, compile orchestration, or how analyzer queries are memoized.

---

## `compiler/omega-mangle`

**Owns:** Omega's standalone linker-symbol grammar and encode/decode implementation.

Key files:

- `symbol.rs` — backend/compiler-independent symbol model.
- `encode.rs` — symbol encoding.
- `demangle.rs` — decoding and human-readable demangling.
- `grammar.rs` — grammar/tag definitions.
- `base62.rs` — encoding helper.
- `bin/omg_demangle.rs` — demangler CLI.

**Boundary:** intentionally does not depend on analyzer/HIR compiler representations. Callers translate their own data into the standalone mangling model.

**Look here when:** changing linker identity, encoded type/path grammar, overload uniqueness, or demangling behavior.

**Related doc:** `docs/10-modules-and-linkage.md`.

---

## `compiler/omega-mir`

**Owns:** the backend-independent control-flow representation between semantic analysis and code generation.

Pipeline position:

```text
CheckedModule -> omega_mir::lower_program -> MirModule -> omega-codegen
```

Key files:

- `mir.rs` — MIR item/expression/module data.
- `body.rs` — CFG body, blocks, locals, terminators.
- `ids.rs` — block/local identities.
- `lower/` — checked-tree -> MIR lowering.
  - `function.rs` — function/body lowering.
  - `item.rs` — item lowering.
  - `place.rs` — place lowering.
- `mangle.rs` — construction of final symbols/linkage using `omega-mangle`.
- `lib.rs` — `lower_program` entry point.

**Important invariants:**

- Monomorphization is already complete before MIR lowering.
- MIR makes control flow explicit as a CFG; most non-control-flow expressions remain tree-shaped.
- Shared backend facts such as final linker symbols/linkage are decided here once and read by all backends.
- Modules lower independently; MIR lowering is not a whole-program semantic pass.

**Primary doc:** `docs/16-mir-and-codegen.md`.

**Default context wall:** backend internals are normally irrelevant to MIR changes unless changing the MIR/backend contract.

---

## `compiler/omega-codegen`

**Owns:** translating MIR into final backend output.

Shared/backend-neutral areas:

- `lib.rs` — backend selection, `CodegenRequest`, output kinds, optimization level.
- `abi.rs` — shared ABI decisions.
- `preflight.rs` — backend-independent preflight rejection/checks.

Backends:

- `cranelift/` — Cranelift implementation.
- `llvm/` — LLVM implementation.

Each backend has mirrored areas such as `function.rs`, `expr.rs`, `place.rs`, `item.rs`, `leaf.rs`, and `vtable.rs`.

**Important boundaries:**

- Codegen consumes already-lowered MIR control flow; it should not reconstruct source-level CFG semantics.
- Final linker symbols/linkage come from MIR rather than being re-derived by each backend.
- Aggregate layout is owned by `omega-analyzer::layout`, not independently by backends.
- Backend-specific target vocabulary stays inside the corresponding backend; `omega-analyzer::Target` is the shared compiler vocabulary.

**Primary doc:** `docs/16-mir-and-codegen.md`.

**Default context wall:** when fixing one backend, do not automatically inspect the other. Compare both only when changing a shared contract or enforcing backend parity.

---

## `compiler/omgc`

**Owns:** the command-line compiler frontend.

`src/main.rs` handles CLI arguments and connects the major phases:

```text
CLI / package roots
    -> omega_driver::Driver
    -> CompiledProgram
    -> omega_mir::lower_program
    -> omega_codegen::generate
    -> object / textual output
```

It also selects target/backend/optimization/output mode and renders diagnostics/progress.

**Look here when:** changing CLI flags, compiler invocation behavior, phase wiring, output handling, or user-facing command-line behavior.

---

# Runtime and libraries

## `runtime/core`

Portable core facilities and primitive-facing language support.

Includes primitives, comparison, iteration/ranges, `Option`, and the platform capability declarations used by higher-level libraries.

Relevant docs include:

- `docs/13-core-library.md`
- `docs/18-for-in-loops.md`
- `docs/21-gaps-and-glue.md`

## `runtime/plat`

Default platform glue. Today it provides platform implementations on top of libc-facing declarations.

Primary doc: `docs/22-platform-glue.md`.

## `runtime/std`

Portable standard-library data structures and facilities built above `core` and platform capabilities.

Includes allocation helpers, formatting/I/O, strings, lists, linked lists, hashing, maps, and sets.

Primary docs:

- `docs/23-standard-library.md`
- `docs/24-console-io.md`

## `runtime/shims`

Target-specific assembly/low-level glue that cannot live as ordinary portable Omega code.

Do not inspect this directory for ordinary language/compiler work unless the behavior crosses the compiler/runtime ABI or target boundary.

---

# Tests and examples

- Most compiler crates contain focused Rust unit/regression tests near the implementation they exercise.
- `examples/` contains end-to-end Omega packages used to exercise full compilation/runtime behavior.
- Root `tests/*.expected` and `.stdin` files support end-to-end `just` recipes.
- `justfile` is the main integration build/test entry point, including Cranelift, LLVM, and mixed-backend scenarios.

When planning verification, prefer the narrowest existing test layer that proves the behavior, then add an end-to-end recipe only when the behavior crosses compiler/runtime/backend boundaries.

---

# Where to look by task

| Task / question | Start here | Usually also relevant | Usually avoid initially |
|---|---|---|---|
| Lexing/token spelling | `omega-parser/src/lexer.rs` | parser diagnostics, `docs/15-*` | analyzer/MIR/codegen |
| Grammar / syntax | `omega-parser/src/parser/` | AST, relevant language doc | MIR/backends |
| Macro behavior | `omega-parser/src/macros.rs` | macro parser, `docs/12-*`, HIR boundary | analyzer unless semantics change |
| Post-expansion representation | `omega-hir` | parser AST, `docs/15-*` | backends |
| Name resolution | `omega-analyzer/src/analysis/paths.rs`, `resolver.rs` | driver resolver/module queries, `docs/10-*` | MIR/codegen |
| Types / semantic checking | `omega-analyzer` | HIR types, relevant feature docs | backend internals |
| Calls / overload resolution | `analysis/calls.rs` | `resolved_type.rs`, generics/specs as required | parser/codegen unless contract changes |
| Specs / conformances | `analysis/specs.rs` | driver conformances, `docs/08-*` | backends initially |
| Generics / monomorphization | analyzer generics + driver item queries | `docs/06-*` | codegen unless representation changes |
| Compile-time evaluation | `comp_eval.rs` | `docs/19-*` | backends |
| Modules/imports/packages | `omega-driver` | analyzer resolver interface, `docs/10-*` | MIR/codegen |
| Layout / ABI representation | analyzer `layout.rs` + codegen `abi.rs` | relevant type docs | parser unless syntax changes |
| Control-flow lowering | `omega-mir/src/lower/` | checked tree, `docs/16-*` | backend internals initially |
| Mangling / linker identity | `omega-mir/src/mangle.rs`, `omega-mangle` | module/linkage docs | parser |
| Backend-independent codegen | codegen root/ABI/preflight | MIR | parser/HIR |
| Cranelift bug | `omega-codegen/src/cranelift/` | MIR contract | LLVM unless shared issue |
| LLVM bug | `omega-codegen/src/llvm/` | MIR contract | Cranelift unless shared issue |
| CLI / compile invocation | `omgc/src/main.rs` | driver/codegen public APIs | compiler internals not implicated |
| Core library behavior | `runtime/core` | relevant docs | compiler unless language behavior is involved |
| Standard library behavior | `runtime/std` | `runtime/core`, `docs/23-*` | compiler unless necessary |
| Platform glue | `runtime/plat`, `runtime/shims` | `docs/21-*`, `docs/22-*` | unrelated compiler passes |

---

# Cross-cutting design principles

These are established project-wide patterns; consult `docs/README.md` and the relevant topic doc before changing them.

- **Structured, span-anchored diagnostics.** Avoid raw diagnostic strings and speculative hints.
- **Monomorphization, not erasure.** Generic code is analyzed per concrete instantiation.
- **Mirror, don't unify by default.** Parallel struct/enum/union pipelines are often intentional rather than accidental duplication.
- **Resolve once; read back everywhere.** Cache semantic decisions at the owning phase instead of re-deriving them later.
- **Root-cause fixes over narrow patches.** Generalize a bug fix to the actual pattern when the evidence supports it.
- **One fact, one owner.** Avoid maintaining the same semantic/grammar/backend decision independently in multiple places.

---

# Documentation routing

`docs/README.md` is the index for current technical documentation. For task work, select topic docs directly rather than reading the entire sequence.

Common routes:

- Functions/calls: `docs/00-functions.md`
- Types/primitives/representation: `docs/01-primitives.md`
- Mutability/storage semantics: `docs/02-variables-and-mutability.md`
- Control flow: `docs/03-control-flow.md`
- Structs/unions: `docs/04-structs-and-unions.md`
- Enums/patterns: `docs/05-enums-and-pattern-matching.md`
- Generics: `docs/06-generics.md`
- Visibility: `docs/07-visibility.md`
- Specs/conformance/dispatch: `docs/08-specs.md`
- Annotations: `docs/09-annotations.md`
- Modules/linkage/mangling: `docs/10-modules-and-linkage.md`
- Strings/casts/slices: `docs/11-strings-casting-and-slices.md`
- Macros: `docs/12-macros.md`
- Core library: `docs/13-core-library.md`
- Current known issues: `docs/14-known-issues.md`
- Parser/HIR architecture: `docs/15-parsing-and-hir.md`
- MIR/codegen architecture: `docs/16-mir-and-codegen.md`
- Architectural/design audit notes: `docs/17-design-review.md`
- Iteration: `docs/18-for-in-loops.md`
- Compile-time evaluation: `docs/19-compile-time-evaluation.md`
- Marker/zero-sized types: `docs/20-marker-types.md`
- Gap/glue platform abstraction: `docs/21-gaps-and-glue.md`
- Default platform layer: `docs/22-platform-glue.md`
- Standard library: `docs/23-standard-library.md`
- Console I/O: `docs/24-console-io.md`

`docs/plan/` contains historical implementation plans. Do not search or read it by default during feature work, cleanup, review, or debugging.

---

# Updating this file

Update `ARCHITECTURE.md` only when a **navigation-level architectural fact** changes, for example:

- a crate/subsystem gains or loses ownership of a responsibility;
- a major pipeline boundary moves;
- a new major crate/backend/runtime layer appears;
- the canonical entry point for a concern changes;
- a new current design document becomes the primary route for a subsystem.

Do **not** expand this file with detailed algorithms, exhaustive symbol lists, historical rationale, feature specifications, or implementation notes. Those belong in source documentation or the relevant topic document.
