# Omega issues and design debt

This directory contains **non-normative exceptions to the intended state**: known compiler/library bugs, unsupported cases, implementation limitations, and architectural debt worth preserving.

- [`known-issues.md`](known-issues.md) — concrete current bugs/limitations.
- [`design-debt.md`](design-debt.md) — unresolved design/architecture inconsistencies that merit future work.
- [`language-limitations.md`](language-limitations.md) — limitations/caveats migrated out of language chapters.
- [`compiler-limitations.md`](compiler-limitations.md) — implementation caveats migrated out of architecture chapters.

Rules:

- Resolved issues should be removed from current issue docs; git history and `docs/plan/` preserve history.
- Do not make a known compiler bug normative by copying it into `docs/language/` as the intended behavior.
- Agents should not load this whole directory by default. Consult the relevant issue file when working in that area or when current behavior contradicts the language/architecture docs.
