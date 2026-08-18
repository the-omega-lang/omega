# Omega — technical documentation

Omega is a statically-typed, compiled systems language (Cranelift and
LLVM backends, own hand-rolled lexer/parser/analyzer). This documentation tracks the
language's **current** design and implementation state for an audience
that already knows how compilers work — it explains *what* the syntax is,
*why* it was built that way, and what's genuinely unfinished or unsound
today. It is not a tutorial.

Update these docs whenever a language feature changes, ships, or a
known-issue is fixed — that's their whole purpose. Each file ends with its
own **Caveats** section; [14-known-issues.md](14-known-issues.md) is a
flat, cross-referenced index of every one of them for a quick scan.

## Design pillars, stated once here rather than repeated per file

- **Rust-quality diagnostics are an explicit, ongoing standard** —
  structured, span-anchored, never a raw string; no hint is offered unless
  it's always true.
- **Monomorphization, not erasure** — a generic is fully re-analyzed per
  concrete instantiation. This single fact is why static dispatch, spec
  bounds, and weak-linkage cross-TU sharing all needed little to no new
  machinery once it existed.
- **Mirror, don't unify.** `struct`/`enum`/`union` are three deliberately
  separate, hand-mirrored item pipelines, not one generalized "aggregate
  type" abstraction — an established, repeated style choice in this
  codebase, not technical debt.
- **Resolve once, at signature time; read back everywhere.** Annotations,
  self-mode, spec visibility inheritance, and mangling all follow this —
  re-deriving a fact at use time instead of caching it once is a recurring
  source of bugs this project has hit and fixed more than once (see the
  `@mangling` + `extern` story in [annotations.md](09-annotations.md)).
- **Root-cause fixes over narrow patches.** When a bug pattern is found,
  the fix generalizes to the whole pattern (see the hidden-field →
  hidden-method generalization in [visibility.md](07-visibility.md)), and
  a fix's own newly-exposed edge cases get tested and flagged before
  shipping, not left for the next bug report.

## Reading order

> **Agent note:** the sequence below is for human onboarding. Coding agents working on a concrete task should **not** read it front-to-back. Start from [`ARCHITECTURE.md`](../ARCHITECTURE.md), then read only the topic documents relevant to the task and search the owning source subsystem.

New to the codebase — read roughly in this order:

1. [Functions](00-functions.md) — declaration grammar, overloading,
   generics, why variadics are C-interop-only.
2. [Primitives & representation](01-primitives.md) — the type set, IR
   leaves, fat pointers.
3. [Variables & mutability](02-variables-and-mutability.md)
4. [Control flow](03-control-flow.md) — `if`/`while`/`for`/`loop`,
   `&&`/`||`/`!` and how they desugar.
5. [Structs & unions](04-structs-and-unions.md)
6. [Enums & pattern matching](05-enums-and-pattern-matching.md) — header/
   dynamic/body fields, `match`, ranges, refinement.
7. [Generics](06-generics.md) — the monomorphization model and its
   confirmed gaps.
8. [Visibility](07-visibility.md) — `exposed`/`internal`/hidden/`reveal`.
9. [Specs](08-specs.md) — interfaces, static + dynamic dispatch,
   primitive extension.
10. [Annotations](09-annotations.md) — `@layout`, `@inline`, `@mangling`,
    `@suppress`, `sizeof<Type>`.
11. [Modules, resolution & linkage](10-modules-and-linkage.md) — imports,
    `--extern`, mangling, weak-linkage symbol sharing.
12. [Strings, casting & slices](11-strings-casting-and-slices.md)
13. [Macros](12-macros.md)
14. [The core library](13-core-library.md)
15. [Known issues tracker](14-known-issues.md)
16. [Parsing, macro expansion & the HIR](15-parsing-and-hir.md) — the front
    half of the pipeline: the lexer/parser, why macro expansion sits between
    two ASTs, why the HIR exists at all, the contextual-keyword registry,
    and the span-ownership rule diagnostics depend on.
17. [The MIR, and how it reaches Cranelift](16-mir-and-codegen.md) — the
    control-flow graph `omega-mir` builds between semantic analysis and
    codegen, why it exists (multi-backend support), and what's deliberately
    still a tree.
18. [Design review](17-design-review.md) — unsoundness, inconsistencies, and
    rough edges found on a deeper audit pass; distinct from the known-issues
    tracker in that most entries here aren't bugs at all, just weak spots.
19. [`for` .. `in` loops](18-for-in-loops.md) — the iteration protocol
    (`Iterator<T>`/`ToIterator<T>`/`Option<T>`), how it desugars, and the
    one narrow ambient-name-resolution exception it needed.
20. [Compile-time evaluation (`comp`)](19-compile-time-evaluation.md) — the
    `comp` interpreter, no-storage `comp` bindings, and const promotion.
21. [Zero-sized types (`marker`)](20-marker-types.md) — a data-free
    declaration that can still implement specs, why `struct`/`union` must
    always hold real data, and what's already free vs. newly built.
22. [Gaps and glue](21-gaps-and-glue.md) — how
    `core`/`std` declares a platform-specific capability (a heap
    allocator, first) with no portable implementation of its own, and how
    exactly one project-wide implementation gets wired to it, deferring an
    unfilled gap to the linker rather than whole-program reachability
    analysis.
23. [`plat`: the default platform glue](22-platform-glue.md) — an
    ordinary `--extern` package, not a special one, that fills `core`'s
    gaps; today just a single `libc`-backed heap allocator, with no
    platform-selection mechanism yet.
24. [The standard library](23-standard-library.md) — `std`'s own data
    structures (`List`, `LinkedList`, `String`, `HashMap`, `HashSet`),
    built the same way on every platform, on top of `core::platform`'s
    allocator gap alone.

## What this is not

Not a language spec, not a tutorial, and not a changelog — it describes the
*current* state only. Rationale for individual decisions is summarized
inline per topic; the fuller blow-by-blow (including false starts and
rolled-back designs like the `@ufcs` annotation predecessor to
[primitive blocks and conformance](08-specs.md)) lives in this repo's own commit history,
where design conversations are recorded verbatim.

- [24. Console I/O](24-console-io.md)
