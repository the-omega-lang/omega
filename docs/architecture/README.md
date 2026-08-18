# Omega compiler architecture documentation

This directory explains **how this implementation realizes Omega**. It is not the language definition.

Start with the repository root [`../../ARCHITECTURE.md`](../../ARCHITECTURE.md), which is deliberately compact and routes tasks to the correct subsystem. Read files here only when a task needs deeper implementation detail.

Current migrated notes:

- [`parsing-and-hir.md`](parsing-and-hir.md) — parser/macro-expansion/HIR ownership and invariants.
- [`mir-and-codegen.md`](mir-and-codegen.md) — MIR structure, backend seam, lowering/codegen responsibilities.
- [`module-driver-and-linkage.md`](module-driver-and-linkage.md) — driver orchestration, module discovery, external roots, symbol/linkage model.
- [`abi-and-representation.md`](abi-and-representation.md) — implementation representation/ABI facts extracted from the old mixed technical docs.

Architecture documentation can grow later as needed; it should not duplicate the normative semantics in `docs/language/`.
