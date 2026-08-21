# Omega compiler architecture

This directory describes **how the current Omega compiler and runtime are structured**. It is implementation documentation, not the language definition.

Use the root [`ARCHITECTURE.md`](../../ARCHITECTURE.md) as the compact map. Open one of these files only when the task needs the deeper mechanism.

## Reading map

| Question | Read |
|---|---|
| How does a compilation move through the repository? | [`compiler-overview.md`](compiler-overview.md) |
| How do lexing, parsing, macro expansion, IDs, and HIR lowering fit together? | [`parsing-and-hir.md`](parsing-and-hir.md) |
| How are packages discovered, items memoized, generics instantiated, and analyzer queries orchestrated? | [`module-driver-and-linkage.md`](module-driver-and-linkage.md) |
| How does semantic analysis resolve names/types/calls/specs and build the checked tree? | [`semantic-analysis.md`](semantic-analysis.md) |
| Where do semantic types, layouts, target widths, and compile-time values live? | [`types-layout-and-const-eval.md`](types-layout-and-const-eval.md) |
| How are control flow, MIR, and LLVM codegen structured? | [`mir-and-codegen.md`](mir-and-codegen.md) |
| What representation/calling-convention facts does codegen rely on? | [`abi-and-representation.md`](abi-and-representation.md) |
| How are linker symbols constructed and where is linkage decided? | [`symbol-mangling.md`](symbol-mangling.md) |
| How do spans/errors/warnings become rendered diagnostics? | [`diagnostics.md`](diagnostics.md) |
| How do `core`, `std`, platform glue, and shims relate to compiler packages? | [`runtime-and-platform.md`](runtime-and-platform.md) |
| Which tests protect which architectural boundaries? | [`testing-and-validation.md`](testing-and-validation.md) |

## Authority and scope

- [`docs/language/`](../language/) is normative for Omega language semantics.
- This directory is authoritative for **intended implementation architecture and ownership**, but source code remains authoritative for exact current mechanics.
- [`docs/issues/`](../issues/) records known deviations, unsupported cases, and design debt. Do not copy those limitations into architecture docs as if they were desired architecture.
- [`docs/plan/`](../plan/) is historical cold storage.

Architecture documents should explain **ownership, phase boundaries, data flow, invariants, cache/query structure, and extension points**. They should not duplicate syntax/semantic rules already specified under `docs/language/`, and they should avoid historical “before/after” narratives unless the history is necessary to explain a current invariant.

## Cross-cutting rules

Several rules recur across the compiler:

1. **One fact, one owner.** Grammar facts belong to the parser; semantic decisions to the analyzer/driver; layout to `omega-analyzer::layout`; ABI to `omega-codegen::abi`; final symbols/linkage to MIR lowering.
2. **Resolve once, read back later.** Later phases should consume already-decided facts rather than re-derive them independently.
3. **Shared decisions stay shared.** Accepted-program checks, layout, ABI, symbols, and linkage are decided once upstream of LLVM emission and must not be re-derived or duplicated inside codegen.
4. **Crate boundaries are real architecture boundaries.** Crossing one should happen through its public data/interface, not by duplicating the other crate's logic.
5. **Determinism is observable.** Any cache/set that feeds IDs, diagnostics, declaration order, symbols, or emitted output must not depend on randomized iteration order.
