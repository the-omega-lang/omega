# Omega — technical documentation

Omega is a statically-typed, compiled systems language (Cranelift backend,
own hand-rolled lexer/parser/analyzer). This documentation tracks the
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
  the fix generalizes to the whole pattern (see the private-field →
  private-method generalization in [visibility.md](07-visibility.md)), and
  a fix's own newly-exposed edge cases get tested and flagged before
  shipping, not left for the next bug report.

## Reading order

New to the codebase — read roughly in this order:

1. [Functions](00-functions.md) — declaration grammar, overloading,
   generics, why variadics are C-interop-only.
2. [Primitives & representation](01-primitives.md) — the type set, IR
   leaves, fat pointers.
3. [Variables & mutability](02-variables-and-mutability.md)
4. [Control flow](03-control-flow.md) — `if`/`while`/`for`, no `&&`/`||`.
5. [Structs & unions](04-structs-and-unions.md)
6. [Enums & pattern matching](05-enums-and-pattern-matching.md) — header/
   dynamic/body fields, `match`, ranges, refinement.
7. [Generics](06-generics.md) — the monomorphization model and its
   confirmed gaps.
8. [Visibility](07-visibility.md) — `exposed`/`internal`/private/`hidden`.
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

## What this is not

Not a language spec, not a tutorial, and not a changelog — it describes the
*current* state only. Rationale for individual decisions is summarized
inline per topic; the fuller blow-by-blow (including false starts and
rolled-back designs like the `@ufcs` annotation predecessor to
[for-attached specs](08-specs.md)) lives in this repo's own commit history,
where design conversations are recorded verbatim.
