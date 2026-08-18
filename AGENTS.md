# Omega agent guide

This file is intentionally short. It contains rules that are useful on nearly every task; detailed project knowledge lives elsewhere and should be loaded only when relevant.

## Start here

1. Read [`ARCHITECTURE.md`](ARCHITECTURE.md) to identify the subsystem that owns the behavior.
2. Read only the topic docs under [`docs/`](docs/) that are relevant to the task.
3. Search the source for concrete symbols before opening large files.
4. Treat crate/subsystem boundaries as default context boundaries. Cross them only for a concrete dependency.
5. Stop exploring once the affected behavior, interfaces, invariants, and verification path are understood well enough to act safely.

## Context discipline

- Context is a budget. Do not inspect files, callers, callees, tests, backends, or neighboring modules merely "for completeness".
- Prefer symbol/reference search and targeted ranges over whole-file reads, especially in large analyzer/parser/backend files.
- Do not recursively traverse dependencies. Follow a dependency only when it answers a specific question required by the task.
- Current source is authoritative for implementation details. Current `docs/` are authoritative for intended/current design and known caveats.
- `docs/plan/` is historical cold storage. Do not search or read it unless current source/docs leave a necessary design rationale unresolved.
- The reading order in `docs/README.md` is for human onboarding, not ordinary agent work.
- Git history is archaeology, not default context. Consult it only when current source/docs do not explain a necessary rationale.
- Do not refactor unrelated code discovered during a task. Record it for follow-up instead.

## Task classes

Use the cheapest workflow that preserves correctness:

- **Local/mechanical:** developer directly. Examples: private rename, bounded cleanup, small test maintenance, formatting-only or comment-only work.
- **Feature/refactor with known desired semantics:** architect -> reviewed `PLAN.md` -> fresh developer context -> reviewer.
- **Unsettled language/architecture design:** thinker -> architect -> reviewed `PLAN.md` -> fresh developer context -> reviewer.
- **Repo-wide maintenance:** define the rule once, partition by crate/directory, and execute isolated batches. Never perform a semantic whole-repository sweep in one context.

For large tasks, prefer fresh contexts between thinking, planning, implementation, and review. Transfer decisions through concise artifacts rather than conversation history.

## Default subsystem walls

- **Syntax/parsing:** `compiler/omega-parser` + relevant language docs. Add HIR only if representation changes.
- **HIR/desugaring:** `compiler/omega-hir` + parser-facing types/docs. Analyzer stays closed unless semantics change.
- **Semantic analysis:** `compiler/omega-analyzer` + the HIR types it consumes + relevant docs. Parser/MIR/codegen stay closed unless their contracts change.
- **Module/package orchestration:** `compiler/omega-driver` + analyzer resolver interfaces + module/linkage docs.
- **MIR:** `compiler/omega-mir` + checked representation it consumes + MIR/codegen docs.
- **Backend emission:** the relevant backend in `compiler/omega-codegen` + MIR interfaces it consumes. Do not automatically inspect both backends unless behavior must remain synchronized.
- **Diagnostics infrastructure:** `compiler/omega-diagnostics`; feature-specific diagnostic construction stays with the owning frontend/semantic crate.
- **Runtime/library:** the relevant tree under `runtime/` + its public contract docs; compiler internals remain closed unless language/compiler support changes.

See `ARCHITECTURE.md` for the full ownership map.

## Omega invariants

- No hidden behavior: no implicit allocation, invisible control flow, or surprise cost.
- No libc requirement for the language/core model.
- Embedded/freestanding use is first-class.
- Stable ABI and first-class C interoperability.
- Abstractions should compile away rather than obscure the machine.
- Prefer simple, intentional architecture over hacks, duplicate mechanisms, or unexplained special cases.

## Repository hygiene

For semantic cleanup across many files (comments, docs, naming, consistency, dead code), work one bounded crate/directory at a time. Keep important rationale where it belongs, but do not turn a cleanup into a documentation redesign or architecture audit unless explicitly requested.
