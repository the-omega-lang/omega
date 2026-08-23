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

Several cross-boundary policies are named data rather than positional booleans/tuples:

- `ResolveItemOptions` names recursive-indirection and deliberate visibility-bypass policy at item lookup sites;
- `OverloadCandidate` names candidate declaration identity, resolved function type, and visibility;
- `ResolvedBound` names the bound target, spec identity, and concrete spec arguments;
- `ResolvedField` / `CheckedField` are distinct from function parameters, so aggregate field semantics are not encoded through parameter-shaped records.

These types are intentionally small: their purpose is to make call sites and invariants readable, not to create another abstraction layer.

## Analyzer construction

`Analyzer::new`/`new_in` receives:

- mutable resolver reference;
- absolute owning module path;
- concrete generic substitution;
- active resolved bounds when needed;
- owning `AnalysisSite { id, span }` for diagnostics and semantic ownership;
- target.

Concrete generic substitutions are inserted as defined types in the initial context. Duplicate generic names are diagnosed at construction.

## Scope/context model

`context.rs` owns lexical scope machinery.

A `Context` tracks nested `LexicalScope`s containing facts such as:

- local variable bindings and storage identities;
- types/generic substitutions visible in scope;
- local compile-time values;
- usage/write information for warnings;
- narrowing/widening state used by control flow/patterns.

Resolution order distinguishes lexical locals/types from module-level names. Module aliases and top-level declarations are not copied wholesale into every analyzer context; the resolver is queried when local scope lookup does not answer the question.

Declared `alias` names are erased during this resolution rather than represented. `omega_analyzer::aliases::normalize_type` is the one canonical alias-application operation, and it works on the **whole** written type rather than only its root: it walks pointers, arrays, function parameters/results, spec conjunction members and generic arguments, and expands every alias application it finds. `apply_alias_once` under it binds one alias template's full effective argument list (explicit arguments plus defaults, using the same default/arity rule ordinary generic items use) and returns that layer's substituted RHS.

The ordering inside `apply_alias_once` is what makes alias-owned `(bound, argument)` obligations complete and non-duplicated: each supplied or defaulted argument is normalized once *before* substitution, and the template body is normalized *before* the arguments are substituted into it, with the alias's own parameters treated as opaque placeholders. The body's obligations then have the final arguments substituted into them. So `alias Duo<T> = Pair<T, T>` checks one written argument once, while `alias Outer<T> = Pair<Inner<T>, T>` still reports `Inner`'s own bound against the real argument -- an alias reachable only through another alias's RHS, default or bound cannot disappear.

`expand_type_alias` (used at the top of `Context::resolve_type`, and again for a pointer's pointee to decide pointer-versus-dynamic-object) and `applied_alias_bounds` (used to check those obligations) are both thin views over this one operation, so expansion and bound-checking cannot diverge. `Analyzer::check_alias_generic_bounds` consumes the obligations directly and does not re-walk the source type looking for nested applications. Path-shaped aliases are instead canonicalized by the driver, which answers each query for the target's own identity. No `ResolvedType`, spec cell, `decl_id`, or checked node is ever created for an alias.

`omega_analyzer::aliases::alias_reference` resolves a written type's head through the same anchor/import machinery an ordinary path uses -- an explicit anchor goes through `ModuleResolver::resolve_explicit_anchor`, and an unqualified head reached through an import (not just a locally-declared name) is followed via `ImportTarget::ItemPath` before falling back to the current module -- and carries the resulting `ItemAccess` (absolute path plus whether visibility was already authorized) into `ModuleResolver::resolve_visible_alias`. The analyzer never asks whether an alias merely *exists*; it asks the accessor-aware query, which is the alias's own visibility gate. A bare reference to a structural alias imported from another module therefore resolves exactly as a locally-declared one does, and a hidden structural alias is rejected wherever it is named.

## Analysis sites and synthetic identity

Top-level analyzer/query entry points carry an explicit `AnalysisSite { id, span }` rather than an untyped `(HirId, Span)` protocol. The site binds semantic ownership and diagnostic anchoring into one value while nested operations can still report against their own child IDs/spans.

- `HirId` is stable identity for semantic caches/references.
- `Span` is diagnostic anchoring.
- `AnalysisSite` is the focused analyzer/query owner.

Synthetic semantic artifacts use fresh IDs minted by the driver under `SYNTHETIC_MODULE`. Desugarings must not reuse one source `HirId` for several distinct checked nodes: the user-written outer node may retain its source ID, while generated temporaries/calls/literals/inner control-flow nodes receive synthetic identity.

## Main semantic concerns

`analysis/` is split by what is being checked rather than by a monolithic pass:

- `items/mod.rs` — top-level signature/type-shape analysis;
- `items/bodies.rs` — top-level body checking and checked aggregate/function construction;
- `stmts.rs` — statements, blocks, return/divergence/loops/defer;
- `exprs/mod.rs` — expression dispatch, place reads, calls/assignments/address-of;
- `exprs/operators.rs` — unary/binary operators, casts/coercions, compound assignment;
- `exprs/ranges.rs` — range construction and synthesized bound calls;
- `literals.rs` — numeric/aggregate/enum/union literal construction;
- `places/mod.rs` — shared place analysis, mutability and place identity;
- `places/roots.rs` — place-root lookup and overload roots;
- `places/fields.rs` — field/index projections and member lookup;
- `places/slicing.rs` — runtime and compile-time slicing;
- `paths.rs` — qualified/unqualified semantic path lookup;
- `calls/mod.rs` — shared callee/receiver/call construction and ordinary/dynamic calls;
- `calls/spec.rs` — spec-qualified/static/instance dispatch;
- `calls/overload.rs` — overload candidate resolution/ranking;
- `calls/generic.rs` — generic inference and concrete generic call completion;
- `patterns.rs` — match pattern analysis, narrowing, exhaustiveness entry points;
- `specs.rs` — specs, bounds, conformances, dynamic dispatch facts;
- `consts.rs` — compile-time expression integration;
- `visibility.rs` — exposed/shared/hidden/reveal checks.

Each module contributes `impl Analyzer` methods; they share the common state and resolver seam rather than constructing parallel analyzers. The splits follow reasoning domains, not arbitrary file-size thresholds.

Temporary analyzer state is also scoped structurally rather than by repeated manual push/pop sequences. Helpers such as `with_scope`, `with_bounds`, `with_loop`, `with_suppressed`, `with_owner`, and `with_defer_body` restore their state after the focused operation. New temporary semantic state should follow the same pattern so early returns cannot leave the analyzer in a half-mutated context.

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

Call analysis is one of the densest semantic junctions, so its implementation is deliberately split by resolution mode rather than kept in one dispatcher file. It combines:

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

HIR already provides a flattened syntactic place chain. `analysis/places/` resolves it into a typed `CheckedPlace`:

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

Specs have canonical declaration cells containing their own raw requirement information -- a spec declares only itself, with no alias/dependency mechanism. A concrete use substitutes spec arguments + `Self` and resolves effective member slots.

A raw `spec A + B + ...` conjunction (`Type::SpecStatic`, parser/HIR-level) becomes semantic only at the point where `Context::resolve_pointer_type` sees it as an *immediate* pointer pointee: `Pointer(SpecStatic(members), mutable)` resolves to `ResolvedType::SpecObject { shape, mutable }`. A bare (non-pointer) `spec ...` in a supported static position is instead normalized into one fresh generic bounded by every member; a bare `spec ...` anywhere else remains a `TypeResolutionError`. This is the one place static-vs-dynamic is decided -- MIR/codegen never see an unresolved `Type::SpecStatic`.

That static-parameter normalization is `omega_analyzer::generics::normalize_static_spec_params`, and it is the single owner of the rule. It expands type-form aliases first, so a literal `spec A + B` parameter and an aliased `AB` parameter produce the same anonymous bounded generic. Every query that reports a function's generic arity, generic signature, collected signature, or body works from its result, so those views cannot diverge.

`shape` is a `ResolvedSpecShape`: a canonicalized list of `ResolvedSpecApplication { spec, spec_args }`. Canonicalization resolves every member to its final spec declaration, normalizes its generic arguments, removes exact duplicate applications, and sorts what remains by fully qualified spec name plus a canonical normalized-argument key -- never by declaration/discovery order, which can vary across compilations. Consequently `*spec A + B` and `*spec B + A` resolve to the same `ResolvedSpecShape` and compare/hash equal; reordering is not a coercion or cast, because there is nothing to convert.

Conformance logic spans analyzer + driver:

- driver: registration, matching templates, goal/cycle lifetime, package ownership;
- analyzer: semantic target/spec resolution, requirement checking, bounds, method compatibility, dynamic-dispatch slot meaning.

This split prevents the analyzer from owning a global conformance database while keeping semantic compatibility logic out of the driver.

Dynamic spec calls emerge as checked nodes with enough resolved slot/callee information for codegen to build/use vtables without re-running conformance selection. For a conjunction object, each canonical shape member's slots are concatenated in shape order (`Analyzer::type_implements_shape`/`finish_dynamic_dispatch_call`) -- this concatenation is exactly the vtable section layout codegen materializes, so a member's compile-time section offset is a sum of the slot counts of the members before it, not a search for its first method (an object-safe spec may legally have zero).

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

Checked aggregate/function items also carry already resolved metadata needed downstream, including concrete type args, self mode, annotations, conformance/primitive ownership, and method bodies. Aggregate fields are represented by `CheckedField`; function parameters by `CheckedParam`. The distinction exists in HIR (`HirField` / `HirParam`) and resolved semantic data (`ResolvedField`) as well, so field visibility/identity is not smuggled through a parameter-shaped tuple.

Range/slice checking uses an explicit `CheckedRangeEnd::{Inclusive, Exclusive, Open}` shape. Impossible combinations such as “inclusive but no end expression” are therefore unrepresentable in the checked tree; MIR currently performs the compatibility flattening required by its older range representation.

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

## Maintainer invariants that are easy to break

Several analyzer behaviors depend on local implementation structure rather than a single language rule:

- Expected-type propagation is deliberately directional in several constructs. Where one operand/branch becomes the inference anchor, later operands are checked against that anchor rather than participating in a global "best type" search. Keep the corresponding language rules in `docs/language/` aligned with any change to this mechanism.
- `ResolvedType::accepts` answers only "is this already the right representation". Conversion into an anonymous enum — member injection and subset widening alike — lives in one analyzer helper instead, because both change tags and can change payload layout. Expected-type coercion and `<enum ...>` cast analysis are two callers of that helper, and overload viability scores with the same compatibility predicate, so neither can accept a value the other rejects. The helper decides the source-index → destination-index remap; it travels in the checked node and MIR expands it into tag/`EnumBody`/`EnumConstruct` control flow rather than recomputing canonical indices.
- A writable alias invalidates flow-sensitive narrowing for the aliased place. Mutable borrow/call paths therefore de-assume refinements before later reads can reuse them.
- Visibility `reveal` is implemented as a scoped bypass owned by `RevealState`. Operand handlers use shared `with_reveal_operand` / `with_reveal_bypass` helpers rather than manipulating raw frame booleans. A successful hidden/shared access marks every active reveal frame used, so nested `reveal` chains do not generate a false redundant-reveal warning. The remaining architectural limitation—place resolution itself does not structurally own reveal activation—is tracked in `docs/issues/`.
- Overload scoring may fully analyze user-written arguments before a winner is chosen. The winning path must reuse that checked work rather than analyze the same expression again and risk duplicate diagnostics or side effects in analyzer bookkeeping.
- Import aliases are re-resolved through the resolver with the actual lookup context when indirection/generic arguments matter. Eager alias snapshots are navigation aids, not a substitute for a context-sensitive item query.
- Reads/writes of projected compile-time values operate on immutable `ConstValue` trees: a projected write rebuilds the root value with the changed leaf rather than mutating real storage.
- For projected runtime places, reaching a field/index/deref generally reads the root even if the final operation is a write; only a projection-less assignment can be treated as a pure write for unused-variable analysis.

These rules belong here rather than in per-function prose. Keep source comments only where the nearby control flow still needs a short explanation of *why that implementation choice exists*.
