# Architectural testing and validation

Omega uses several test layers. The right layer depends on which architectural boundary the change touches.

## Rust unit/regression tests

Compiler crates contain focused tests near the implementation and/or under crate `tests/` directories.

Use these for facts that can be proven without linking/running a full Omega program, for example:

- lexer/parser grammar;
- macro expansion;
- HIR lowering;
- module discovery;
- mangling round trips;
- layout/ABI helpers;
- focused driver semantic diagnostics;
- MIR shape.

A compiler error test should assert the intended diagnostic/reason, not merely “compilation failed”.

## Driver integration tests

`compiler/omega-driver/tests/` exercises source through package parsing/semantic compilation and often MIR lowering without requiring the whole external link/runtime stack.

This is a good boundary for language/compiler semantics that involve multiple frontend/semantic components.

## End-to-end examples

`examples/` contains actual Omega packages. The root `justfile` compiles selected examples into objects, links them, runs them, and checks exit/output/symbol properties.

Use this layer when correctness depends on one of:

- generated native code;
- runtime/core/std behavior;
- object linking;
- separate compilation;
- gap/glue reachability;
- symbol names/linkage;
- backend equivalence.

## Separate-compilation validation

Package identity/mangling/weak linkage bugs often cannot be caught by compiling one package in one process.

Relevant recipes compile dependencies and applications through separate `omgc` processes, then link the objects. This protects:

- declared module identity;
- extern references;
- generic instantiation ownership;
- duplicate weak folding;
- concrete strong-definition uniqueness.

## Mixed-backend validation

Because ABI, layout, symbols, and linkage live above backend-specific emission, objects from Cranelift and LLVM are intended to interoperate.

A change to a shared external contract should include a mixed-backend link/run gate where possible.

A backend-local arithmetic/instruction bug normally needs only that backend plus the shared MIR contract; do not run an unrelated repo-wide matrix for every local backend fix.

## Runtime capability validation

The just recipes deliberately link different subsets of runtime objects.

Examples of architectural assertions include:

- core-only code links without std/platform;
- allocator-using std code can link without console platform code when console paths are unreachable;
- console code requires the appropriate glue;
- section garbage collection removes unreachable functions/capability references.

Link success/failure itself can be the primary assertion; symbol inspection (`nm`, `readelf`) can provide a secondary structural check.

## Symbol/mangling validation

Use `omega-mangle` encode/decode round-trip tests for grammar changes, plus object-level symbol inspection for compiler adapter changes.

Especially test:

- root `main` special case;
- nested same-name functions;
- overload signatures;
- generic args;
- conformance/primitive methods;
- gap/glue identity;
- weak duplicate instantiations.

## Backend IR verification

LLVM modules are explicitly verified before output. A verifier failure is treated as an internal compiler error.

Cranelift's construction APIs perform their own structural validation during compilation.

Backend-native verification is not a substitute for semantic tests: it proves IR well-formedness, not that the compiler emitted the intended program.

## Documentation consistency

When architecture changes:

1. update the owning deep architecture doc;
2. update root `ARCHITECTURE.md` only if routing/ownership/pipeline changed;
3. update `AGENTS.md` only if the general context workflow/authority rule changed;
4. update language docs only if observable semantics changed;
5. add/remove issue entries for unresolved/resolved deviations.

Do not write the same implementation invariant into every layer.

## Verification selection

Prefer an expanding sequence:

```text
focused unit/regression
    -> owning crate tests
    -> driver integration
    -> one end-to-end recipe
    -> cross-backend/separate-compilation matrix only if contract requires it
```

The goal is not the maximum number of commands; it is the smallest validation set that actually crosses every changed boundary.
