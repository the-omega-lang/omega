# Omega agent guide

This file is intentionally short. It contains rules useful on nearly every task; detailed project knowledge lives in the repository documentation and should be loaded only when relevant.

## Start here

1. Read [`ARCHITECTURE.md`](ARCHITECTURE.md) to identify the subsystem that owns the behavior.
2. Choose the **smallest relevant documentation layer** from [`docs/README.md`](docs/README.md); do not survey `docs/` broadly.
3. Search the source for concrete symbols before opening large files.
4. Treat crate/subsystem boundaries as default context boundaries. Cross them only for a concrete dependency.
5. Stop exploring once the affected behavior, interfaces, invariants, and verification path are understood well enough to act safely.

## Documentation authority and routing

Omega deliberately separates language semantics from implementation knowledge:

- **Writing/modifying `.omg`:** start with [`docs/guide/quick-reference.md`](docs/guide/quick-reference.md). Do **not** infer Omega syntax from Rust, C, C++, or another similar language.
- **Exact language semantics:** [`docs/language/`](docs/language/) is the normative **Omega Language Specification**. Read only the chapter(s) relevant to the task.
- **Compiler/runtime implementation:** start with [`ARCHITECTURE.md`](ARCHITECTURE.md), then use [`docs/architecture/README.md`](docs/architecture/README.md) to select the one or two deep architecture documents needed. Do not survey the architecture directory broadly.
- **Programmer/library usage:** [`docs/guide/`](docs/guide/) is explanatory and example-oriented; it does not override `docs/language/`.
- **Known bugs/limitations/design debt:** [`docs/issues/`](docs/issues/) is non-normative exception/debt tracking. Consult relevant entries when working in that area or when observed behavior conflicts with the specification; do not load it globally by default.
- **Historical plans:** [`docs/plan/`](docs/plan/) is cold storage. Read it only when current docs/source leave a necessary rationale unresolved.

If `docs/language/` and current compiler behavior disagree, first check `docs/issues/`. Source is authoritative for what the implementation **currently does**; the language specification is authoritative for what Omega **is intended to mean**. Do not silently turn an implementation bug into language semantics or silently “fix” a specification without establishing which side is stale.

## Context discipline

- Context is a budget. Do not inspect files, callers, callees, tests, backends, or neighboring modules merely “for completeness”.
- Prefer symbol/reference search and targeted ranges over whole-file reads, especially in large analyzer/parser/backend files.
- Do not recursively traverse dependencies. Follow a dependency only when it answers a specific question required by the task.
- Git history is archaeology, not default context. Consult it only when current source/docs do not explain necessary rationale.
- Do not refactor unrelated code discovered during a task. Record a real follow-up issue instead.

## Task classes

Use the cheapest workflow that preserves correctness:

- **Local/mechanical:** developer directly. Examples: private rename, bounded cleanup, small test maintenance, formatting/comment-only work.
- **Feature/refactor with known desired semantics:** architect -> reviewed `PLAN.md` -> fresh developer context -> reviewer.
- **Unsettled language/architecture design:** thinker -> architect -> reviewed `PLAN.md` -> fresh developer context -> reviewer.
- **Repo-wide maintenance:** define the rule once, partition by crate/directory, and execute isolated batches. Never perform a semantic whole-repository sweep in one context.

For large tasks, prefer fresh contexts between thinking, planning, implementation, and review. Transfer decisions through concise artifacts rather than conversation history.

## Default subsystem walls

- **Syntax/parsing:** `compiler/omega-parser` + relevant `docs/language/` grammar/feature chapter. Add HIR only if representation changes.
- **HIR/desugaring:** `compiler/omega-hir` + parser-facing types + `docs/architecture/parsing-and-hir.md`. Analyzer stays closed unless semantics change.
- **Semantic analysis:** `compiler/omega-analyzer` + HIR types it consumes + relevant `docs/language/` + `docs/architecture/semantic-analysis.md` when implementation mechanics matter. Parser/MIR/codegen stay closed unless their contracts change.
- **Module/package orchestration:** `compiler/omega-driver` + analyzer resolver interfaces + `docs/language/modules-and-imports.md` + `docs/architecture/module-driver-and-linkage.md` as needed.
- **MIR:** `compiler/omega-mir` + checked representation it consumes + `docs/architecture/mir-and-codegen.md`.
- **Backend emission:** relevant backend in `compiler/omega-codegen` + MIR interfaces + `docs/architecture/mir-and-codegen.md`; add `docs/architecture/abi-and-representation.md` only for shared representation/calling-convention work. Do not automatically inspect both backends unless behavior must remain synchronized.
- **Diagnostics infrastructure:** `compiler/omega-diagnostics` + `docs/architecture/diagnostics.md`; feature-specific diagnostic construction stays with the owning frontend/semantic crate.
- **Runtime/library:** relevant tree under `runtime/` + `docs/architecture/runtime-and-platform.md` + relevant guide/language docs. Compiler internals remain closed unless language/compiler support changes.

See `ARCHITECTURE.md` for the full ownership map.

## Testing and validation

Use [`docs/architecture/testing-and-validation.md`](docs/architecture/testing-and-validation.md) for the detailed test-layer map. The routine rules are:

- **Compiler component behavior:** use the owning crate's Rust tests (`src/.../tests.rs` for implementation-detail tests, `<crate>/tests/` for black-box/component integration tests).
- **Language conformance / executable semantics:** use the root [`tests/`](tests/) suite. Each direct child `tests/<case>/` is an Omega package consumed by [`bin/test-runner`](bin/test-runner); optional `expected.stdout` / `expected.stderr` files define exact expected output. Observable language behavior should be tested here against the rule in `docs/language/`, not only through a compiler-internal unit test.
- **Focused conformance run:** after `omgc` and the runtime objects are already built, run `./bin/test-runner <case> [...]`. The runner uses `target/` by default; set `OMEGA_ARTIFACTS_DIR` when the built runtime objects live elsewhere.
- **Normal full gate:** `just test-all` builds the required compiler/runtime artifacts and runs the conformance runner.
- **Verification scope:** start with the smallest focused test that crosses the changed boundary, then expand only when the change affects wider contracts such as runtime/linking, separate compilation, or backend parity.
- **Negative tests:** a compile failure only proves the intended behavior when the expected diagnostic output is checked; never accept "it failed" as sufficient.

Do not infer language semantics from an existing test when it conflicts with `docs/language/`; the specification remains normative and the mismatch must be resolved as a compiler/test/docs issue.

## Omega invariants

- No hidden behavior: no implicit allocation, invisible control flow, or surprise cost.
- No libc requirement for the language/core model.
- Embedded/freestanding use is first-class.
- Stable ABI and first-class C interoperability.
- Abstractions should compile away rather than obscure the machine.
- Prefer simple, intentional architecture over hacks, duplicate mechanisms, or unexplained special cases.

## Comment policy

Comments are part of the context budget. Prefer code whose structure and names make the implementation understandable without narration. Add or keep a comment only when removing it would hide information that is difficult or unreasonable to express in code.

**Good comments explain:**

- *why* a non-obvious choice exists, especially when the obvious alternative is wrong;
- a local invariant, precondition, aliasing/lifetime assumption, safety argument, or subtle algorithmic constraint;
- an external ABI/platform/toolchain requirement that directly constrains the nearby code;
- a deliberately surprising workaround that cannot yet be removed, preferably with a pointer to the relevant `docs/issues/` entry when the problem is durable.

**Do not add or preserve comments that:**

- restate what the next line/function/type already says;
- narrate control flow step by step;
- act as section headings for straightforward code that should instead be structured with functions/types/modules;
- preserve dead code, old implementations, debugging notes, or historical narrative;
- duplicate language semantics or architectural knowledge already owned by current documentation;
- speculate about possible future work without a concrete, durable issue.

Documentation comments (`///`, `//!`) are not exempt: keep them when they document a real public/internal contract that users or callers need, but do not rustdoc every private helper merely to describe its name or signature.

When a comment contains durable knowledge, put the knowledge at the narrowest correct home:

- language semantics -> `docs/language/`;
- compiler/runtime architecture or cross-module invariants -> `docs/architecture/`;
- user-facing usage -> `docs/guide/`;
- unresolved bug/limitation/debt -> `docs/issues/`;
- line-level/local implementation reasoning -> keep a concise comment beside the code.

Do **not** move every useful comment into documentation. Local reasoning that only makes sense next to a particular branch, unsafe operation, representation trick, or algorithm belongs next to that code. Conversely, do not use source comments as a second specification or architecture manual.

For comment cleanup, never optimize for a comment percentage or delete comments mechanically. Work in bounded crate/directory batches, classify each comment by purpose, remove low-signal narration/duplication, migrate only genuinely durable knowledge, and verify that behavior is unchanged.

## Documentation maintenance

When behavior changes, update the layer that owns the fact:

- language semantics -> `docs/language/`;
- compiler/runtime architecture -> `docs/architecture/`;
- user-facing usage/examples -> `docs/guide/`;
- unresolved bug/limitation/design debt -> `docs/issues/`.

Do not add “temporary caveats” to normative language chapters for compiler bugs. Do not preserve resolved issue narratives in current docs; git history and `docs/plan/` already provide history.

For semantic cleanup across many files (comments, docs, naming, consistency, dead code), work one bounded crate/directory at a time. Do not turn cleanup into a documentation redesign or architecture audit unless explicitly requested.
