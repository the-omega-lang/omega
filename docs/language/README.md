# Omega Language Specification

This directory is the normative, living definition of the Omega language implemented by this repository. Its purpose is stronger than a programmer guide: it should contain enough syntax and semantic information for an independent implementation to accept the same programs and give them the same observable meaning.

This is currently an **implementation specification**, not a versioned standards document. It describes the language as it exists now. Unsupported or intentionally incomplete behavior belongs under [`../issues/`](../issues/), not as ambiguous caveats mixed into normative chapters.

## Normative conventions

- **must / shall**: required behavior for a conforming implementation.
- **must not / shall not**: forbidden behavior.
- **may**: implementation freedom that does not change observable semantics.
- Code examples are illustrative; prose rules are normative unless a chapter explicitly states otherwise.
- Compiler-internal Rust type names, passes, caches, and file paths are non-normative and belong in [`../architecture/`](../architecture/).

The word `spec` in Omega source means the interface-like language construct. “Language Specification” refers to this documentation set.

## Index

### Source text and grammar

1. [`lexical-structure.md`](lexical-structure.md) — source characters, identifiers, keywords, comments, literals, punctuation.
2. [`grammar.md`](grammar.md) — program/item/statement/expression/type grammar and syntactic restrictions.
3. [`modules-and-imports.md`](modules-and-imports.md) — package/module identity, source layout, imports, roots, ambient `core` names.
4. [`visibility.md`](visibility.md) — hidden/exposed/internal items and members, `reveal`.

### Types and declarations

5. [`types-and-primitives.md`](types-and-primitives.md) — primitive set, pointers, arrays, slices, literals, `never`, size/layout-visible rules.
6. [`bindings-and-mutability.md`](bindings-and-mutability.md) — declarations, inference, `mut`, scope, receiver forms.
7. [`functions.md`](functions.md) — functions, returns, overloading, generics, `defer`, C-only variadics.
8. [`structs-and-unions.md`](structs-and-unions.md) — aggregates, fields, literals, methods.
9. [`enums-and-pattern-matching.md`](enums-and-pattern-matching.md) — enum representation-visible semantics, construction, narrowing, matching.
10. [`marker-types.md`](marker-types.md) — zero-sized `marker` declarations and values.
11. [`generics.md`](generics.md) — generic declarations, bounds, inference, defaults, monomorphization semantics.
12. [`specs-and-conformance.md`](specs-and-conformance.md) — `spec`, aliases, `conform`, blanket conformance, static/dynamic dispatch.
13. [`gaps-and-glue.md`](gaps-and-glue.md) — platform/runtime capability declarations and implementations.
14. [`foreign-function-interface.md`](foreign-function-interface.md) — `extern`, variadics, symbol control, C-facing entry/linkage contracts.

### Expressions and execution

15. [`control-flow-and-operators.md`](control-flow-and-operators.md) — `if`, `match`, loops, boolean/operators, precedence and inference.
16. [`iteration-and-ranges.md`](iteration-and-ranges.md) — ranges and `for .. in` iteration protocol.
17. [`strings-casts-arrays-and-slices.md`](strings-casts-arrays-and-slices.md) — string/byte-string semantics, casts, fixed arrays and slices.
18. [`compile-time-evaluation.md`](compile-time-evaluation.md) — `comp` bindings/expressions and compile-time evaluator semantics.
19. [`annotations-and-sizeof.md`](annotations-and-sizeof.md) — annotations and `sizeof<Type>`.
20. [`macros.md`](macros.md) — declarative token macros, parameters, repetition, hygiene and visibility.

## Completeness rule

A language feature is not considered fully documented until the relevant chapter states:

- its accepted source forms;
- name/scope/visibility behavior where applicable;
- typing and inference rules;
- runtime/compile-time semantics;
- observable layout/ABI behavior where the language promises it;
- rejection rules for meaningful invalid forms.

When a compiler change alters language behavior, update this directory in the same change. Known implementation failures belong in `docs/issues/`; do not encode bugs as normative semantics merely because the current compiler exhibits them.
