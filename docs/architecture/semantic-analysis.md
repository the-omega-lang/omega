# Semantic analysis

`omega-analyzer` implements Omega's semantic rules. It consumes HIR plus queries supplied by `omega-driver`, and produces resolved semantic types and a checked tree suitable for MIR lowering.

The most important boundary is:

> `omega-analyzer` owns semantic algorithms; `omega-driver` owns module/filesystem/query lifetime.

## Analyzer lifetime

`analysis::Analyzer` is intentionally short-lived. One instance analyzes one focused top-level signature/body (or an equivalent focused semantic operation), then `finish()` returns its accumulated findings and field-usage information.

It stores only analysis-local state:

- lexical scopes / variable bindings / defined generic types;
- current return type;
- loop stack + break facts;
- active `defer` restrictions;
- active suppression/reveal state;
- current aggregate owner for hidden member access;
- generic/spec bounds available in the body;
- target;
- a borrowed `ModuleResolver`.

It does **not** retain a whole package's modules or filesystem cache.

## The resolver boundary

All module-shaped questions go through `ModuleResolver`.

The analyzer can ask for:

- import/module/name resolution;
- visibility facts;
- raw generic declarations for inference;
- overload candidates;
- canonical spec declarations;
- primitive methods;
- conformance proofs/candidates;
- other function bodies for compile-time evaluation;
- known `comp` declaration values;
- synthetic IDs.

The driver implements this interface with its item/module/conformance query caches.

This dependency direction lets analyzer unit logic remain independent of filesystem discovery and prevents semantic code from growing a second module cache.

## Analyzer construction

`Analyzer::new`/`new_in` receives:

- mutable resolver reference;
- absolute owning module path;
- concrete generic substitution;
- active resolved bounds when needed;
- owning HIR `(HirId, Span)` for diagnostics;
- target.

Concrete generic substitutions are inserted as defined types in the initial context. Duplicate generic names are diagnosed at construction.

## Scope/context model

`context.rs` owns lexical scope machinery.

A `Context` tracks nested `ScopeContext`s containing facts such as:

- local variable bindings and storage identities;
- types/generic substitutions visible in scope;
- local compile-time values;
- usage/write information for warnings;
- narrowing/widening state used by control flow/patterns.

Resolution order distinguishes lexical locals/types from module-level names. Module aliases and top-level declarations are not copied wholesale into every analyzer context; the resolver is queried when local scope lookup does not answer the question.

## Node identity convention

Semantic operations generally carry the explicit `(HirId, Span)` of the source construct being checked.

- `HirId` is stable identity for semantic caches/references.
- `Span` is diagnostic anchoring.

The pair is not hidden in one global “current node” because nested analysis frequently needs to report against a child while retaining parent context.

Synthetic semantic artifacts use IDs minted by the driver under `SYNTHETIC_MODULE`.

## Main semantic concerns

`analysis/` is split by what is being checked rather than by a monolithic pass:

- `items.rs` — top-level signatures and bodies;
- `stmts.rs` — statements, blocks, return/divergence/loops/defer;
- `exprs.rs` — general expression typing/operators;
- `literals.rs` — numeric/aggregate/enum/union literal construction;
- `places.rs` — addressable places, projections, mutability, slicing;
- `paths.rs` — qualified/unqualified semantic path lookup;
- `calls.rs` — call resolution, overloads, methods, generics, dynamic calls;
- `patterns.rs` — match pattern analysis, narrowing, exhaustiveness entry points;
- `specs.rs` — specs, bounds, conformances, dynamic dispatch facts;
- `consts.rs` — compile-time expression integration;
- `visibility.rs` — exposed/internal/hidden/reveal checks.

Each file contributes `impl Analyzer` methods; they share the common state and resolver seam rather than constructing parallel analyzers.

## Signature first, bodies second

The driver invokes analyzer entry points in two broad phases.

### Signature collection

Signature analysis establishes facts later users must be able to reference independent of declaration order:

- value/function types;
- aggregate identity + fields/method signatures;
- generic parameter/bound shape;
- annotations whose semantic meaning is needed later;
- spec declaration/member information;
- declaration visibility;
- mangling/inline/layout-related resolved annotations.

The project pattern is **resolve once at signature time, read back later**. Do not re-parse/re-resolve an annotation or method identity independently during body checking/codegen.

### Body checking

Body analysis consumes the already resolved signature and checks:

- local declarations and scope;
- expression/call/place types;
- returns/divergence;
- assignments/mutability;
- control flow;
- pattern refinement/exhaustiveness;
- spec/conformance calls;
- compile-time expressions;
- warnings/field use.

It produces `Checked*` nodes carrying already resolved types and semantic decisions.

## `ResolvedType`

`resolved_type.rs` is the semantic type vocabulary shared by analyzer, driver, MIR, and codegen.

It contains primitive/scalar forms plus compound forms such as:

- pointers, slices, strings, unsized/sized arrays;
- function types;
- struct/enum/union nominal cells;
- specs/spec objects;
- refined enum variants;
- `never`/`void` distinctions.

Nominal aggregates use shared `Rc<RefCell<Resolved*Type>>` cells. Checked nodes therefore refer to the same canonical semantic declaration/layout/method information instead of embedding diverging copies.

More detail: [`types-layout-and-const-eval.md`](types-layout-and-const-eval.md).

## Calls

Call analysis is one of the densest semantic junctions because it combines:

- ordinary functions;
- overload groups;
- generic inference + concrete instantiation;
- inherent methods / static functions;
- primitive methods;
- spec/conformance methods;
- explicit qualified spec calls;
- dynamic dispatch through spec objects;
- implicit receiver shaping/auto-reference rules;
- argument compatibility and variadics.

The key architecture rule is that a call must emerge from analysis with its target and type facts decided. MIR/codegen should not repeat overload or method lookup.

When generic type arguments are omitted, call analysis may ask the resolver for a raw generic signature, infer type arguments from written argument shapes, and then resolve the concrete instantiated item through the ordinary query model.

## Places and storage semantics

HIR already provides a flattened syntactic place chain. `analysis/places.rs` resolves it into a typed `CheckedPlace`:

- concrete root identity/storage kind;
- resolved type after every projection;
- field/variant identity/index;
- mutability/addressability constraints;
- alignment information used downstream.

Assignments/address-of/method receiver handling then consume the same checked place facts.

The analyzer decides *whether* a place operation is legal. MIR/codegen decide *how* the resolved place maps to local/global/backend storage.

## Generics

Omega uses monomorphization, not type erasure.

`generics.rs` provides structural unification/inference helpers. The driver supplies raw generic declaration shapes for inference and owns the concrete `ItemKey(module, name, type_args)` query.

Analysis of a concrete instantiation runs with concrete type substitutions in the initial context. The resulting checked item is concrete and can be lowered/emitted without unresolved type parameters.

Bounds are resolved/expanded and available to body checking. Conformance lookup can use those bounds as proof context rather than demanding a concrete global conformance for every generic operation.

## Specs and conformances

Specs have canonical declaration cells containing raw member/dependency information. A concrete use substitutes spec arguments + `Self` and resolves effective member slots.

Conformance logic spans analyzer + driver:

- driver: registration, matching templates, goal/cycle lifetime, package ownership;
- analyzer: semantic target/spec resolution, requirement checking, bounds, method compatibility, dynamic-dispatch slot meaning.

This split prevents the analyzer from owning a global conformance database while keeping semantic compatibility logic out of the driver.

Dynamic spec calls emerge as checked nodes with enough resolved slot/callee information for codegen to build/use vtables without re-running conformance selection.

## Pattern matching and exhaustiveness

Pattern analysis resolves pattern meaning and narrows the scrutinee where appropriate.

`exhaustiveness.rs` contains interval-domain coverage machinery used for finite/integer-like domains. Enum-variant matching also has dedicated enum-shape logic because variant identity provides a different exhaustiveness domain than matching the numeric tag field directly.

Known conceptual inconsistencies belong in [`../issues/design-debt.md`](../issues/design-debt.md), not in the normative architecture contract.

## Compile-time evaluation integration

Analysis may evaluate `comp` expressions once their semantic dependencies are resolved. Successful values become `ConstValue` and can replace runtime subtrees in the checked representation.

The evaluator can request checked function bodies and known comp declaration values through resolver callbacks rather than bypassing the query architecture.

More detail: [`types-layout-and-const-eval.md`](types-layout-and-const-eval.md).

## Checked tree contract

`checked.rs` is the semantic output vocabulary.

A `CheckedExprNode` carries:

```text
HirId
Span
ResolvedType
CheckedExpr kind
```

Control-flow statements retain source-level/tree structure at this point (`CheckedIf`, `CheckedMatch`, loops, `return`, `defer`, etc.). MIR is responsible for converting that control flow into a CFG.

Checked aggregate/function items also carry already resolved metadata needed downstream, including concrete type args, self mode, annotations, conformance/primitive ownership, and method bodies.

## Errors and warnings

Semantic findings remain structured `AnalysisError` / `AnalysisWarning` variants. They are not pre-rendered terminal strings.

Analyzer-local `error(...)`/warning helpers attach IDs/spans and suppression state. The driver later stores/drains findings by module; the CLI renderer handles source snippets and terminal styling.

This separation lets semantic code describe **what is wrong** while `omega-diagnostics` owns **how a diagnostic looks**.

## Dead-code/usage information

Normal checked-tree traversal can collect declaration/member usage. Compile-time-evaluated subtrees may disappear into `CheckedExpr::Const`, so the analyzer separately records field/variant usage observed during compile-time evaluation and returns it from `finish()`.

The driver merges both sources before whole-program/local-package dead-code warnings.

## Change routing

- Name lookup/import interaction -> analyzer paths + driver resolver.
- Local scope/mutability -> analyzer context/places/statements.
- Call/generic inference -> analyzer calls/generics + targeted driver query interface.
- New semantic type -> `ResolvedType`, analyzer rules, layout/ABI only if representation changes.
- New source control-flow construct -> analyzer checked shape, then MIR lowering.
- New global/package semantic relationship -> usually analyzer rule + driver-owned registry/query lifetime.
