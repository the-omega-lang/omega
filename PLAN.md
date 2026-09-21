# Explicit generic overload selection

## Task Description

- **Deliverable:** Add `f<Type: A + B>(...)` and `f<spec A + B>(...)` to select generic function declarations by their declared bounds, with consistent support for calls and function values.
- **Purpose:** Make overlapping generic overloads explicitly selectable without runtime machinery or automatic preference for stricter bounds.
- **Chosen direction:** Treat each selector as a positional constraint on a function's own generic parameter. Match the exact canonical declared bound set; independently infer/check the concrete type and prove its conformances. Remove strict-bound-superset ranking from function selection. Retain existing argument adaptation costs and concrete-over-generic tie-breaking.
- **Planning interpretations:** The request's unbounded preference applies only to explicitly written plain type arguments, not inferred positions. With multiple such positions, compare their unbounded-position sets by strict inclusion; conflicting preferences remain ambiguous. These two extensions were raised for clarification and are the proposed defaults for this plan.
- **Rejected alternatives:** Bounds as runtime casts; selectors as new instantiated types; choosing the first declaration; automatically preferring `A + B` over `A`; selecting a function by instantiating every candidate and suppressing its errors. Blanket-conformance precedence remains its own existing rule.

## Technical Details

### Language contract

Written function generic arguments remain a positional prefix. Each entry consumes one parameter position:

| Entry | Type/value binding | Overload constraint |
|---|---|---|
| `M` in a type slot | Fix that slot to `M` | Prefer an unbounded slot under the rule below |
| `M: A + B` | Fix that type slot to `M` | Require exactly the declared bound set `A + B` |
| `spec A + B` | Leave that type slot for ordinary inference/defaults | Require exactly the declared bound set `A + B` |
| Existing `comp` argument | Bind normally | No bound selector or unbounded preference |

Selectors are not conformance assertions that add bounds to a declaration. `f<M: A>` cannot select `f<T>` or `f<T: A + B>`, even when `M` implements both specs. A selector never waives conformance checking. If no declaration matches the selector, report that failure without falling back to another overload.

Resolve selector names in the caller's normal visibility/hygiene context; resolve declaration bounds in their declaration/owner context. Expand spec-conjunction aliases and compare sets by spec identity and canonical generic arguments, ignoring member order and duplicates. For example, `A + B`, `B + A`, and an alias of that conjunction select identically. Do not expand blanket entailment into extra declared bounds. Compare dependent declaration bounds after substitution, including owner parameters, `Self`, and generic spec type/`comp` arguments and defaults. Selectors do not supply inverse inference from bound expressions or search for an implementing type.

`spec A` leaves an inference hole while still occupying its position, so `f<spec A, u8>(...)` is legal. Later omitted positions retain ordinary inference/default behavior. An expected result or expected function type may determine the hole. With no source of a concrete type and no applicable default, report inability to infer it; a unique selected bound set is not enough. Existing restrictions on defaults establishing a parameter/signature match remain in force.

Selection proceeds as follows:

1. Discover the existing authorized candidate set, respecting namespaces, imports, aliases, visibility, and inherent-method precedence. Any generic arguments on the function segment exclude concrete declarations.
2. For each candidate, bind written concrete arguments, retain selector holes, infer from the existing argument/expected-type rules, and check parameter compatibility. Reject selector use in a `comp` slot, excess arguments, and incompatible kinds. A selector does not change argument types or adaptation costs.
3. Complete only remaining arguments eligible for ordinary defaults, match selectors against the substituted declared bounds, and prove all required nominal conformances. An ordinary absent conformance makes the candidate inapplicable. A malformed declaration or cyclic/ambiguous conformance proof remains a real error, not evidence for falling back. Do not instantiate candidate function bodies during this work.
4. Among applicable candidates, retain minimum adaptation cost (function values instead require exact signature matching). Preserve the existing concrete-over-generic tie-breaker.
5. Among tied generics, let `U(c)` contain the explicitly written **plain type** positions at which candidate `c` declares no bounds. Candidate `a` defeats `b` only if `U(a)` strictly contains `U(b)`. Selector positions, omitted positions, and `comp` positions contribute nothing. Retain all undominated candidates: one wins, none is a no-match error, and more than one is ambiguous. Never rank nonempty bound sets by strength.
6. Materialize only the winner through the existing declaration-and-arguments instantiation path. A subsequent body error does not trigger fallback.

Consequences to document and test:

- With only `f<T: A>` and `f<T: B>`, `f<M>()` selects A when M implements only A, and is ambiguous when M implements both.
- Adding `f<T: A + B>` does not resolve that ambiguity. Explicit selectors select A, B, or A+B exactly.
- With `f<T>` and `f<T: A>`, `f<M>()` prefers the unbounded declaration; `f<M: A>()` selects the bounded one. An inferred `f(M{})` is ambiguous if both apply at equal cost.
- With `f<T, U: B>` and `f<T: A, U>`, `f<M, M>(...)` is ambiguous when both apply: neither unbounded-position set contains the other. A written prefix affects only its own positions.
- Inapplicable declarations cannot block a viable one, including a bounded minimum-cost candidate whose conformance is absent. Selection may therefore reach a higher-cost applicable candidate.
- More than one declaration can survive the same exact selector, including dependent bounds that become identical after substitution. Ordinary compatibility/cost rules still apply; unresolved ties are errors.

Support the syntax wherever function generic arguments already work: free/imported/aliased/module-qualified functions, inherent static/member calls, `Owner::self::method`, and uncalled function values. Function values use the same selector and preference policy, with their existing exact-signature requirement. Written selectors also apply to singleton generic declarations; fast paths must not ignore them.

Selectors are legal only on a function's generic argument list. Reject them on aggregate constructors, owner/type/spec applications, generic defaults, and function-pointer values. Nested ordinary types such as `*spec A` remain ordinary type arguments, not selectors. Keep the existing one-written-generic-list-per-path restriction. Do not extend unsupported generic owner inference or generic declarations in `meet`/`primitive` blocks.

### Initial context boundary and affected sites

Start with `docs/language/{functions,generics,grammar,specs-and-conformance}.md`, `docs/guide/quick-reference.md`, and `docs/architecture/semantic-analysis.md`. The primary owner is `omega-analyzer`; parser/HIR interfaces change to preserve selector syntax, and driver queries change to support applicability without function instantiation.

- **Parser/HIR:** `compiler/omega-parser/src/ast/{generics,identifier,expression}.rs`, `src/prelude.rs`, and `src/parser/expression.rs` (`try_parse_generic_args`, `try_parse_member_generic_args`). Introduce an expression-generic-argument representation with ordinary `GenericArg`, typed selector, and inferred selector forms, preserving selector spans and path provenance. Change `ExprPath.generic_args` and `FieldAccessExpr.generic_args`; carry the representation through `compiler/omega-hir/src/hir.rs` (`HirProjection::FieldAccess`) and `src/lower/expression.rs`. `src/macros/expander.rs` already carries member arguments and must preserve the new forms. Keep type-level `GenericArg` and `Type::Generic` unchanged. Convert ordinary expression entries at owner/type/constructor boundaries, explicitly rejecting selectors there.
- **Analyzer selection:** `compiler/omega-analyzer/src/analysis/calls/overload.rs::resolve_overload_candidates`, `calls/value.rs` (`select_function_value`, `explicit_bindings`, `select_unconstrained`, `value_dominates`), and dispatch in `calls/{mod,generic,spec}.rs`, `analysis/paths.rs`, and `analysis/literals.rs`. Factor shared argument preparation, selector matching, applicability, and generic preference into a small shared calls module; retain the separate call-conversion and exact-value-signature matchers. Update singleton routes as well as overload routes.
- **Bound representation:** `analysis/calls/pattern.rs::overload_template` currently records `(parameter index, spec HirId, argument patterns)` in `resolver.rs::OverloadTemplate.bounds`. Reuse that declaration identity/canonicalization model and `generics/pattern.rs`, adding the resolved-bound view needed for substituted selectors and proofs. Preserve original parameter identity for redeclaration checking; two distinct declarations becoming equal after substitution are not declaration-time duplicates.
- **Driver boundary:** `compiler/omega-analyzer/src/resolver.rs::ModuleResolver`, `compiler/omega-driver/src/resolver.rs::instantiate_overload`, `src/items/resolution.rs` (`complete_overload_arguments`, `check_generic_bounds_under`), and `src/items/methods.rs`. Add a declaration-keyed preparation/applicability query returning completed arguments and canonical declared bounds, or a structured rejection/error, without calling `ensure_item_at` for the candidate function. Reuse declaration-module and enclosing-owner substitution rather than reconstructing it at the call site. Both selection and final instantiation must use the same argument/default and bound semantics.
- **Conformance proof:** `analysis/specs.rs::{check_generic_bound,type_implements_spec}`, driver `src/conformances/{solver,registration}.rs`, and the resolver's `conformance_for` boundary. Use goal-directed nominal proof, distinguishing an absent witness from proof failure. `type_implements_spec` currently has a method-matching fallback that can vacuously accept an empty spec; that cannot establish generic-bound applicability. Align generic-bound checking with the normative requirement for an actual conformance witness, without redesigning dynamic method matching. Preserve conjunction expansion and existing blanket precedence.
- **Diagnostics:** `compiler/omega-analyzer/src/error/{kind,render}.rs`. Preserve ordinary no-match/ambiguity diagnostics, with selector spans, useful candidate bounds, and a disambiguation hint when expressible. Distinguish invalid selector placement/kind, unresolved or non-spec names, exact-bound mismatch, failed conformance, and undetermined selector type. For multiple candidates, retain rejection reasons for the final error without emitting each losing candidate's error.

### Interfaces, invariants, and limits

- Selection metadata ends before checked calls/function values. Resolved generic arguments, instantiation/cache keys, function addresses, mangling, MIR, ABI, and codegen stay unchanged.
- Conformance proofs may use existing lazy conformance queries. Losing **function** bodies must remain unmaterialized. Defaults required to decide applicability may now be resolved for losing candidates; unrelated function bodies and unnecessary defaults must not be checked. Update the old documentation claim that only the winner's bounds/defaults are examined.
- Do not implement speculative proof by truncating global diagnostics or rolling back diagnostics while retaining failed cache entries. Return ordinary non-applicability structurally; propagate genuine proof errors consistently. Declaration order must not affect selection or diagnostic ordering.
- Keep `generics::compare_bound_sets` for declaration duplicates and blanket-conformance ordering. Remove its strict-superset use only from function ranking.
- **Out of scope:** general specialization, structural parameter specificity, new type-inference searches, function/owner dual argument lists, runtime dispatch, broader method-discovery cleanup, and unrelated limitations listed in `docs/issues/language-limitations.md`. Existing unsupported paths must reject selectors clearly rather than silently discard them.
- **Escalation:** Stop if implementation requires changing blanket precedence, introducing selector-dependent instantiation identities, or broadening supported generic declaration positions. Those would change this design's scope.

## Implementation Plan

1. Add the expression-only argument representation and parser support for both forms and conjunctions. Preserve generic/comparison rollback, nested closing angles, and member-call commitment. Thread it through AST expansion/HIR and convert ordinary entries at existing consumers; reject unsupported placements. Add focused parser tests before semantic changes.
2. Establish shared nominal applicability and declaration preparation queries. Reuse existing default resolution, owner substitution, canonical spec arguments, and goal-directed conformance solving. Return completed arguments/bounds and distinguish ordinary rejection from hard proof failure. Cover empty specs, defaults, owner substitutions, and non-materialization at the component layer.
3. Implement common selector preparation and exact set comparison in analyzer calls. Integrate calls, singleton routes, and function values; preserve sparse positional bindings for `spec` holes. Replace function-bound specificity with the stated preference rule and check conformance before final selection. Keep argument adaptation and exact function-value matching separate.
4. Add selector-aware diagnostics and end-to-end cases. Update existing assertions that deliberately encode the superseded rules: the `bound(...)` call in `tests/t05d_generic_overload_ranking`, `rank` function value in `tests/t05h_generic_function_values`, and failed-bound fallback in `tests/t05g_generic_overload_failures`. Use selectors to retain the first two cases' intended successful targets; move the now-successful fallback behavior to a positive case. Revise `compiler/omega-driver/tests/generic_methods.rs::a_generic_member_value_reports_the_winners_own_bound_failure` to assert the new applicability rule and retain a separate explicit-selector bound-failure case.
5. Update normative grammar, calls/function values, and inference/default/selection rules in `docs/language/{grammar,functions,generics}.md`; keep the nominal-bound rule in `specs-and-conformance.md` and remove only function-ranking dependence on blanket precedence. Add short usage examples to `docs/guide/quick-reference.md` and document preparation/proof/instantiation ownership in `docs/architecture/semantic-analysis.md`. Do not change blanket semantics or add compiler caveats to the normative chapters.
6. Run focused component and conformance checks, then the full gates below. Review the diff against the selection algorithm, singleton paths, cache identity, and losing-candidate isolation before handing off completion.

## Testing

### New and changed cases

Add focused root packages (proposed new names) `tests/t05j_generic_bound_selectors/` and `tests/t05k_generic_bound_selector_errors/`, with exact `expected.stdout` and `expected.stderr` respectively. Split negative fixtures further if early parse errors prevent semantic diagnostics from being exercised.

- **Positive selection:** The user's marker with both A and B and observable distinct function bodies; explicit typed and inferred A/B/A+B selections; reordered/aliased conjunctions; generic spec arguments including defaults and `comp` arguments; a type implementing only A selects A without annotation; plain-type unbounded preference; failed bounded candidate falls back; higher-cost viable fallback.
- **Inference and contexts:** Mixed selectors/plain/`comp` entries and an omitted suffix; selector hole followed by a concrete type; expected result and expected function type inference; allowed unused-parameter defaults; lone generic functions; module/import/function aliases; inherent static/member/unbound calls and values, including owner substitution; macro expansion carrying selectors. Verify the same selected declaration/arguments share a function address across spelling variants.
- **Ambiguity:** Both A and B, A versus A+B, inferred unbounded versus bounded, incomparable unbounded-position sets, and multiple surviving declarations after exact selection. Reorder declarations to prove selection is order-independent. Neither selectors nor defaults may introduce structural-specificity ranking.
- **Negative semantics:** Exact selector mismatch (including selecting A when only A+B exists); absent nominal witness, especially an empty spec with no `meet`; type does not satisfy selected bounds; no inference source; defaults cannot manufacture argument/signature compatibility; selector in a `comp` slot; non-spec/inaccessible/unknown selector names; excess arguments; selector on a non-function application or nongeneric function. Assert each intended diagnostic, not merely compile failure.
- **Isolation and proof errors:** Losing function with an invalid body remains unchecked; required defaults and bounds are evaluated in the declaration/owner context; absent conformance emits no speculative error; cyclic/ambiguous conformance is not silently treated as absence. Preserve blanket precedence and duplicate-declaration rejection.

Use `compiler/omega-parser/src/parser/expression/tests.rs` and `compiler/omega-parser/tests/generics.rs` for parsing shapes, malformed lists, nested specs, and comparison rollback. Use focused driver integration tests beside `tests/generic_methods.rs`, `tests/comp_generics.rs`, and `tests/conform.rs` for resolver/proof contracts. Use root packages for observable language behavior; component tests alone are insufficient.

**Specification trace:** Selector syntax maps to `docs/language/grammar.md`; positional holes, inference/default authority, and application restrictions to `generics.md`; applicability/cost/preference/ambiguity and function values to `functions.md`; nominal proof and unchanged blanket ordering to `specs-and-conformance.md`.

**Regression commands:**

```sh
cargo test -p omega-parser -p omega-hir -p omega-analyzer -p omega-driver
```

After the compiler/runtime artifacts are built, run:

```sh
./bin/test-runner t05j_generic_bound_selectors t05k_generic_bound_selector_errors t05b_generic_overload_errors t05c_generic_overload_declarations t05d_generic_overload_ranking t05e_generic_overload_ambiguity t05f_generic_overload_duplicates t05g_generic_overload_failures t05h_generic_function_values t05i_generic_function_value_errors t10_generics t10b_explicit_generic_errors t10c_generic_member_functions t10d_generic_member_function_errors t11b_generic_spec_requirements t34_comp_generics t34b_comp_generic_errors
just test-all
```

The host gate exercises actual emitted selections and freestanding runtime linkage. No backend or ABI change is planned, so a new target/linkage matrix is unnecessary unless implementation reveals a changed downstream contract.
