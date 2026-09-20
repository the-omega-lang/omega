# Generic function value selection

## Task Description

- **Deliverable:** Allow an expected function type to select and instantiate a generic function, including from a mixed overload set. Explicit function arguments such as `thing<i32>` restrict selection to generic declarations. Produce an ordinary function value pointing to the selected monomorphization.
- **Purpose:** Make generic functions addressable and overloads disambiguatable without wrappers, allocation, or runtime selection.
- **Chosen direction:** Extend semantic function-value selection using existing authorized candidates, generic type patterns, specificity, and instantiation queries. Function values require exact signature identity; calls retain their existing conversion-cost gate.
- **Rejected alternatives:** Do not synthesize calls or argument expressions to select a value; do not instantiate every candidate and inspect the results; do not prefer a concrete call candidate before considering conversion cost; do not introduce parameter-structure specialization.

The required behavior for `thing(a: i32) => void` and `thing<T>(a: T) => void` is:

| Use | Selection |
| --- | --- |
| `a: (i32) => void = thing;` | Concrete declaration |
| `a: (i32) => void = thing<i32>;` | Generic declaration with `T = i32` |
| `a: (u32) => void = thing;` | Generic declaration with `T = u32` |
| `thing(10u32);` | Generic declaration, with no conversion |

## Technical Details

**Initial context boundary:** `compiler/omega-analyzer` value paths, overload inference, and patterns; the directly used candidate/instantiation queries in `compiler/omega-driver`; `docs/language/functions.md`, `docs/language/generics.md`, and the Functions subsection of `docs/issues/language-limitations.md`. Read `docs/guide/quick-reference.md` before writing fixtures. Parser/HIR and MIR/codegen need no representation changes.

**Current evidence and affected sites:**

- `analysis/places/roots.rs::resolve_bare_overload_root` and `analysis/paths.rs::resolve_qualified_value` discard template candidates. `paths.rs::resolve_type_member` / `select_uncalled_function` use concrete methods and diagnose uninstantiated generic methods.
- `paths.rs::resolve_generic_args_place` already handles explicit standalone generic item values, but resolves an item rather than selecting its overload declaration. `tests/t10_generics` already executes `pick := sum<i32>;`. The documentation's blanket prohibition on generic values therefore overstates current behavior; preserve that existing success while replacing the restriction deliberately.
- `paths.rs::unique_overload_signature_match` hand-compares signature fields and omits calling convention. Replace it with complete function-type identity in the unified value selection path. `ResolvedFunctionType` equality already ignores parameter descriptors through `ResolvedFunctionParam` equality; `unbound_value()` removes declaration-only receiver metadata.
- `analysis/calls/overload.rs::resolve_overload_candidates` already implements explicit-prefix filtering, inference, conversion cost, specificity, and winner-only `instantiate_overload`. Factor only the reusable candidate operations; its argument analysis and conversion/coercion remain call-specific.
- `generics/pattern.rs::TypePattern::{infer,resolved,exact}` and `analysis/calls/pattern.rs::overload_template` supply structural inference without candidate instantiation. `resolved` intentionally cannot materialize every nominal/spec-object pattern; matching must not require it to do so. `exact` can fall through to compatibility logic, including anonymous-enum subset matching: value selection needs recursively exact matching, not representation acceptance or conversion.
- `resolver.rs::{OverloadCandidate,OverloadTemplate,ModuleResolver}` in the analyzer defines the boundary. Driver `resolver.rs::{raw_overload_signatures,resolve_overload_set,instantiate_overload}` supplies authorized candidates and declaration-specific instantiation. Driver `items/methods.rs::{collect_method_overloads,instantiate_method_overload}` handles concrete-owner methods. Both candidate collectors currently omit singleton generic templates from their overload path. Driver `items/resolution.rs::complete_overload_arguments` supplies remaining defaults after selection.

**Selection contract:**

1. Resolve names and authorization as today. Bare names, module-qualified names, imported/declared aliases, and supported static/unbound member paths use the same selection rules. Preserve local shadowing, alias-frozen candidate sets, `reveal`, inherent/conformance precedence, and static/member namespace separation.
2. A generic list on the **function segment** excludes all concrete declarations, even if every generic candidate fails. A list on an owner, as in `Holder<i32>::self::pick`, instantiates the owner and does not filter out its concrete methods. Keep the existing one-list-per-path restriction; an alias of a concrete owner supports writing the function's own list.
3. Written generic arguments bind the positional prefix with the existing type/`comp` kind rules. They are authoritative. Infer unwritten arguments from the expected function's result and parameters using existing structural inference capabilities; include the explicit receiver parameter of an unbound member. Conflicting occurrences reject the candidate rather than replacing bindings. Validate/canonicalize inferred `comp` arguments as in calls.
4. With an expected function type, require exact parameter count/types, return type, calling convention, and variadic status after substitution and receiver normalization. No numeric/literal adaptation, pointer mutability weakening, anonymous-enum injection/widening, receiver adaptation, variance, or generated adapters apply. Nested function and nominal generic arguments must also match exactly. Parameter descriptors do not affect selection. Inference does not invent anonymous-enum types or add inverse inference beyond the existing generic rules.
5. Defaults do not rescue an unmatched signature during candidate probing. Infer signature-relevant parameters from the expected type first; an otherwise unknown parameter may remain pending only if it has a declared default and is not needed to establish the match. Complete those defaults for the selected declaration using existing declaration-context substitution. Explicit values and expected-type inference outrank defaults. Invalid selected defaults/bounds are errors, never fallback triggers.
6. Among exact matches, use existing specificity: concrete beats generic; among generics, the existing strict bound-set ordering decides. Parameter structure is not a tie-breaker. Equally specific or incomparable survivors are ambiguous. Check the winner's bounds normally; failed bounds must not cause fallback to a less specific declaration.
7. With no expected function type, preserve bare-reference behavior; this change does not guess instantiations for an unconstrained bare generic name. Explicit references such as `f<i32>` can yield a value without an annotation when the explicit prefix leaves a unique eligible declaration and all remaining arguments can be completed from defaults. Multiple eligible declarations still require an expected type; an incomplete singleton reports missing inference information. Preserve already-working fully explicit standalone values and existing defaulted standalone item behavior.
8. Instantiate only the selected generic declaration through its existing identity/cache path, then verify its completed value signature against the expectation and emit the existing `CheckedPlaceRoot::Variable` with `Storage::Function`. Calls and values of the same declaration/arguments must share a monomorphization. Losing generic bodies, bounds, and defaults must not be diagnosed or materialized.

**Interfaces/invariants:** Candidate discovery is driver-owned; matching/ranking is analyzer-owned. Add narrowly scoped function-value candidate queries, reusing driver collection/authorization internals, so singleton templates can participate without changing when calls enter the overload resolver. In particular, routing singleton calls through overload resolution would alter the documented singleton default-inference behavior. Shared ranking accepts an already viable candidate set: calls supply minimum-cost candidates, values supply exact matches. If template signature metadata is needed, carry it explicitly in `OverloadTemplate` rather than discarding ABI distinctions. Visibility failures remain failures, not opportunities to retry through a less restricted lookup path.

**Out of scope:** New syntax, bound receiver closures, simultaneous inference of owner/function generics, generic declarations in currently unsupported `meet`/`primitive` blocks, new specialization rules, overload declaration legality, changes to call conversions, ABI/mangling changes, or bidirectional inference for an overloaded outer call's unresolved callback argument. Ordinary contexts that already propagate a known function type should benefit from the new selector.

**Risks/open questions:** No unresolved language decision blocks this plan. Escalate if implementation requires speculative candidate instantiation, weakening alias/visibility rules, or new owner inference. Do not silently expand this task to fix unrelated documented qualified-call limitations.

## Implementation Plan

1. Extend candidate access at the analyzer/driver boundary for function values, including single generic declarations and concrete-owner static/member templates. Reuse alias canonicalization and authorization from `resolve_overload_set`; reuse declaration keys and method owner substitution. Preserve existing call query behavior and the singleton/overload distinction. Add component coverage proving singleton discovery, authorized alias candidates, and shared instantiation identity.
2. Add a dedicated analyzer function-value selector near the existing overload logic. Extract common explicit-argument binding, `comp` normalization, specificity, and candidate descriptions where needed. Implement expected-signature inference followed by recursive exact matching; ensure nominal, nested function, and anonymous-enum patterns are checked without instantiating losing candidates. Keep call cost computation/coercion unchanged. Use complete signature equality for concrete candidates and the final instantiated winner.
3. Route bare, qualified, explicitly generic, and supported type-qualified references through this selector in `places/roots.rs` and `paths.rs`, including singleton paths. Distinguish owner arguments from function arguments with existing `ExprPath::args_at`; parser/HIR already carry the required information. Preserve non-function path behavior and unbound receiver normalization. Remove superseded concrete-only selection logic rather than maintaining competing implementations.
4. Provide deterministic diagnostics: no exact match, ambiguity with relevant generic candidate descriptions, explicit arity/kind/conflict errors, and insufficient inference information. Reuse existing diagnostics where accurate; retain a useful uninstantiated-generic diagnostic when no type information is available. A rejected speculative candidate emits no independent error; a selected declaration's bound/default/body failure is reported without fallback.
5. Update `docs/language/functions.md` and `generics.md` with the selection contract and the three requested examples, distinguishing value identity from call conversion costs. Remove the resolved expected-function-type restriction in `docs/issues/language-limitations.md`, keeping unrelated limitations. Update the specific lazy-instantiation paragraphs in `docs/architecture/semantic-analysis.md` to cover selected function values and remove the obsolete claim that multiple method templates are always rejected. Avoid a broader documentation rewrite.
6. Add the focused tests below, run component and conformance verification, then review the diff against winner-only instantiation, exact signature identity, and unchanged call priority. Implementation ends with the normal full gate.

## Testing

**New/changed cases:** Add new root packages `tests/t05h_generic_function_values/` and `tests/t05i_generic_function_value_errors/` (proposed new paths). Positive fixtures must call selected values and assert labeled `expected.stdout` output so they distinguish implementations, not merely compile assignments.

- Prove all three requested assignments, with different observable results for concrete and generic bodies. Also test explicit selection without an annotation, a singleton generic inferred from context, return-only inference, a partial explicit prefix completed from context, unused defaulted parameters, and explicit/default precedence.
- Exercise `comp` inference from a fixed-array signature and an explicit `comp` prefix; structural nominal and nested-function signatures; descriptors that differ only in names.
- Exercise bare/module-qualified/imported/declared-alias references and concrete-owner static/unbound member references. Include owner-only generic arguments with mixed method overloads and a concrete-owner alias with explicit function arguments. Call unbound values with the receiver as parameter zero.
- Prove bound-set specificity, absence of diagnostics from a losing generic body, and value/call reuse of one declaration instantiation. Use driver assertions for identity/materialization where execution cannot prove that contract.
- Prove expected-type propagation through assignment and a known callback parameter or returned function value, using existing propagation paths.

**Negative/diagnostic cases:** Check exact `expected.stderr` for the new negative root package; use component assertions for additional diagnostic/identity contracts. Cover equally specific and incomparable generics, structurally different templates that both fit, incompatible explicit arguments with no concrete fallback, excessive/wrong-kind generic arguments, conflicting repeated type constraints, missing inference, no exact signature match, and an unsatisfied selected bound despite a less-specific alternative. Reject convention/variadic mismatches, nested pointer mutability differences, and anonymous-enum widening. Verify inaccessible candidates remain inaccessible through aliases. Keep the existing bare `Holder::self::alone` failure in `t10d_generic_member_function_errors`; it still has neither explicit arguments nor an expected type.

**Specification trace:** Positive and negative value tests implement the revised Generics/Overloading sections of `functions.md` and inference/explicit-prefix/default rules of `generics.md`; unbound tests implement the existing receiver-as-first-parameter contract. Exact ABI/type identity follows `functions.md` and `foreign-function-interface.md`.

**Regression coverage:** Extend `tests/t05d_generic_overload_ranking` with the user's suffixed `u32` call and retain its genuine conversion-cost check (`i64` concrete versus generic with unsuffixed `10`). Both matter: a suffixed literal is already rigid under `generics.md`, so that example alone does not prove conversion remains last resort. Preserve its concrete-at-equal-cost and explicit-generic-call checks. Cover default-sensitive singleton calls from `t10_generics`. Extend driver `tests/generic_methods.rs`, `diagnostics.rs`, and `aliases.rs` only for their relevant contracts; the existing alias value-selection test is `an_overload_selected_as_a_value_uses_the_same_frozen_set_as_a_call`.

**Commands/target coverage:**

```sh
cargo test -p omega-analyzer
cargo test -p omega-driver
./bin/test-runner t05h_generic_function_values t05i_generic_function_value_errors t05d_generic_overload_ranking t05b_generic_overload_errors t05e_generic_overload_ambiguity t05f_generic_overload_duplicates t05g_generic_overload_failures t10_generics t10c_generic_member_functions t10d_generic_member_function_errors t29_type_function_namespaces t30_function_type_descriptors t34_comp_generics
just test-all
```

The focused runner requires rebuilt compiler/runtime artifacts; use the build recipes in `justfile` first when needed. Standard host compile/link/run coverage verifies address materialization with the existing freestanding runtime. No additional platform or separate-compilation matrix is required unless the implementation changes identity/linkage contracts, which this design avoids.
