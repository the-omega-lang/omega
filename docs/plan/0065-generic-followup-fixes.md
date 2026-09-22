# Complete generic overload selection

## Task Description

- **Deliverable:** Fix the two outstanding review findings: distinct generic declarations must remain distinct after emission and separate linking, and invalid caller-written bound selectors must never be accepted through diagnostic suppression. Retain the already implemented syntax and selection policy.
- **Purpose:** Make the original A/B example work with identical signatures and prevent separately compiled callers from silently executing the wrong body.
- **Chosen direction:** Add a canonical, structural identity for a generic function declaration before its own generic parameters are substituted; carry it into the mangled function path. Separately, validate selectors once in the caller's context and use the validated result during candidate matching.
- **Approved ABI migration:** The user approved rebuilding all Omega packages/runtime objects with the updated compiler. Generic function symbols may change. No legacy compatibility aliases or dual exports are required because an old symbol cannot identify both declarations. Compatibility with existing compiled generic objects is not a requirement; no further migration approval is needed.
- **Rejected alternatives:** Encoding `HirId`, source offsets, declaration ordinals, selector spelling, debug/display strings, or the set of visible overloads; using only substituted bounds/signatures; appending an unchecked hash; changing weak definitions to strong ones; ignoring collisions; discarding diagnostics to simulate candidate failure.

## Technical Details

### Current state and scope

The working tree already contains the initial implementation and review's alias fix; the preceding root `PLAN.md` is no longer present. This is a follow-up, not a reimplementation.

The current `omega-mir::mangle::free_function_symbol` uses only the function path, concrete generic arguments, and concrete signature. Two declarations selected as `pick<M: A>` and `pick<M: B>` collide when their signatures agree. A single compilation rejects the duplicate in codegen. Independently compiled clients instead emit identical weak symbol names and link successfully to one body: the review reproduced `10 10` where `10 20` was required.

`Analyzer::try_written_generics` wraps all written-list resolution in `without_diagnostics`. A bounded alias can report a failed obligation yet return a resolved spec; its error is erased and the candidate remains usable. For `alias Restricted<U: A> = Holds<U>`, `Restricted<i32>` must be rejected when `i32` lacks A, regardless of candidate count. Unknown selector names also lose their useful lookup diagnostic.

The review already fixed loss of applied arguments in spec aliases by expanding aliases in `bound_key`, with the `spec-alias` regression in `tests/t05j_generic_bound_selectors`. Preserve this fix and its tests.

Start within analyzer calls/generic patterns and driver preparation for validation; analyzer checked-function metadata, driver body assembly, MIR mangling, and `omega-mangle` for symbol identity. Read `docs/architecture/symbol-mangling.md`, the relevant calls/query sections of `semantic-analysis.md` and `module-driver-and-linkage.md`, and `docs/language/{functions,generics,aliases}.md`. The two review findings are recorded in `docs/issues/compiler-limitations.md`.

### Declaration identity and mangling

Add a typed semantic template identity with these contents:

1. The ordered effective generic parameter list, including type versus `comp` kind and each `comp` parameter's declared value-type pattern.
2. Each type parameter's declared bound set. A bound contains its full declared package/module/spec path and symbolic generic argument patterns.
3. Parameter and return-type patterns, plus calling convention/variadic information. Receiver namespace remains represented by the existing associated-function path; preserve any declaration distinction that existing receiver rules require.

Use positional references for the function's own generic parameters, preserving references through pointers, arrays/lengths, nominal applications, nested function types, spec objects, and anonymous enums. Resolve a method's owner context and `Self` consistently with its existing owner instantiation; the owner path and concrete owner arguments still identify that context. **Do not substitute the function's own parameters when constructing this identity.** For example, a bound involving a parameter and a bound fixed to `i32` must not become the same template identity just because that invocation substitutes `i32`.

Normalize aliases (including generic aliases and conjunction aliases), spec defaults, equivalent compile-time arguments, and static-spec parameter sugar using the existing semantic machinery. Deduplicate/sort unordered bound/conjunction members by stable structural keys, never `HirId`, allocation address, or discovery order. Refer to `type_key::{structural_key,spec_application_key}` for the existing ordering contract; extend symbolic-pattern ordering deliberately instead of serializing its current internal strings as an ABI format. A complete spec identity includes its package and module, even when another module declares the same short name.

Generic parameter names, parameter descriptors, source order, source-file location, bodies, visibility, and the function generic parameters' own default expressions are not identity components. Defaults determine concrete arguments through the existing preparation path; once those arguments are fixed, adding or changing a default must not rename that same declaration/instantiation. Bound-spec defaults still normalize the bound itself. Do not base identity on how many overloads happen to exist or be visible.

Build/cache this descriptor for every instantiated function with **its own effective generic parameters**, including singleton functions and supported generic methods. Do not require a singleton to participate in overload matching merely to obtain an identity. Reuse normalization/pattern-building primitives from `analysis/calls/pattern.rs::overload_template` and `generics/pattern.rs`, but keep the descriptor independent of diagnostic descriptions and proof-time IDs. If the matching pattern model needs a stable spec path, obtain it while the resolved spec is available; do not reconstruct names from local IDs downstream.

Carry the descriptor in `CheckedFunctionDef` as optional template metadata, populated by driver body assembly. Existing internal item/cache keys remain unchanged. Concrete declarations and nongeneric methods on generic owners have no new descriptor and retain their existing symbol forms.

Extend the standalone `omega-mangle` model with a dedicated template-qualified function path node and a typed template descriptor. Encode it **before** the concrete function generic application; keep the concrete ABI signature on `Symbol` as today. Give symbolic type/value parameter references explicit tags and positions, and encode the full descriptor with an unambiguous grammar. Fixed type/value leaves reuse existing mangling vocabulary. Use centralized tags and length/list delimiters; no vendor-suffix payload, invented identifier segment, or lossy fingerprint.

Update encoder, decoder, and demangler together. New demangled output must expose enough declaration information to distinguish A and B; malformed/truncated descriptors, invalid parameter references, and illegal backreferences must be rejected. Existing non-template encodings and decoding of old symbols remain supported, although old and new generic object files are not a supported binary combination under the approved migration.

Definitions and references to one declaration with identical concrete arguments must produce exactly one name, however the caller selected it. Different declarations must produce different names, including across separate invocations and different physical checkout paths using the same declared package identity. Preserve weak linkage for repeated instantiations of the **same** declaration. Do not make codegen invent names or use link order to choose behavior.

Audit free functions, inherent static/member functions, `Owner::self::method`, and any already supported generic requirement/conformance-method emission carrying own function arguments. All such paths must consume the same metadata. `ExternFunctionRef` currently represents nongeneric external declarations; keep that distinction explicit and verify imported generic instantiations go through the emitted checked-body path. Do not silently pass an empty descriptor for a generic reference.

Preserve exact/disabled `@symbol` policy, root `_omg_main`, globals, nominal type names, nongeneric primitive/conformance functions, gaps/glue, and vtables. The separate pre-existing omission of spec module paths in conformance/vtable symbols is not part of this change; the **new descriptor's bound identities** must nevertheless use complete paths.

### Validate selectors before candidate probing

Split written generic-list handling into two stages:

1. **Caller validation:** Resolve each selector once, validate visibility and all alias-owned obligations against the original written syntax, then expand/canonicalize it. In particular, check an alias's obligations before conjunction expansion erases its wrapper. Preserve caller/macro provenance and selector spans. A typed selector's concrete type is also caller-owned and can be checked as a type here; ordinary plain arguments remain kind-dependent until a candidate's parameter kind is known.
2. **Candidate binding:** Apply the validated entries positionally to each declaration, checking arity/kind, retaining inference holes, inferring/defaulting normally, and comparing exact bound sets. Candidate-specific mismatches return structured rejections. They must not resolve selector names or recheck selector alias constraints.

Share this path between `resolve_overload_candidates`, `select_function_value`, and singleton routes through `resolve_written_generics`/`check_written_selectors`. Selection with or without an expected function type must use the same validated selector data. Adding, removing, hiding, or reordering candidates must not change whether a selector is valid or multiply its diagnostic.

Make validation failure explicit. `check_alias_generic_bounds` currently reports errors through side effects and returns no success status; give the selector-facing path a reliable failure result and make `bound_key`/selector validation stop on it. Merely receiving `Some(value)` is insufficient if obligations failed. Preserve the reported cause, return a terminal failed result to the relevant call/value interception path, and do not fall through to ordinary call resolution afterward. Do not truncate diagnostic arrays or retain a successful prepared value after failed validation.

Keep speculative **candidate** errors separate from hard caller/proof errors. A `comp`-slot/kind mismatch, missing inferred argument, or exact-bound mismatch may reject one candidate. Missing nominal conformance is ordinary non-applicability; a genuine malformed declaration or failed/cyclic proof is still an error under existing rules. Do not redesign the conformance solver or change specificity, costs, defaults, inference sources, or visibility policy.

### Verified implementation sites

- `compiler/omega-analyzer/src/analysis/calls/selector.rs`: `WrittenGenerics`, `try_written_generics`, `resolve_written_generics`, `resolve_bound_selector`, `bound_key`, `check_written_selectors`; split validated caller data from candidate bindings.
- `compiler/omega-analyzer/src/analysis/calls/{overload,value,generic,mod}.rs`: call/value candidate entry points and singleton integration. `analysis/mod.rs::check_alias_generic_bounds` and `aliases.rs::{expand_bounds,applied_alias_bounds}` own the alias obligations being lost.
- `compiler/omega-analyzer/src/analysis/calls/pattern.rs`, `generics/pattern.rs`, and `checked.rs::CheckedFunctionDef`: semantic pattern construction and carried template identity. A focused new analyzer module for this descriptor is acceptable; it must not create a second language normalizer.
- `compiler/omega-driver/src/bodies.rs::check_item_body` and `src/items/methods.rs::check_method_body`: attach the descriptor under the declaration/owner context before own generic substitution is lost. `src/items/preparation.rs` and `src/diagnostics.rs::AnalyzerRun` are the preparation/failure boundaries; never ignore a failed analyzer run when retaining prepared data.
- `compiler/omega-mir/src/mangle.rs`, `src/mangle/semantic.rs`, and `src/lower/item.rs`: adapt the descriptor and apply symbol policy consistently. `compiler/omega-driver/src/compile/{mod,output}.rs` and `checked.rs::ExternFunctionRef` are the imported-definition/reference audit boundary.
- `compiler/omega-mangle/src/{symbol,grammar,encode,decode,display,lib}.rs`: standalone model, grammar, tooling, and exports. `tests/roundtrip.rs` and MIR's existing mangling tests cover the protocol.

**Out of scope:** New selector syntax or ranking rules, new generic declaration positions, parser/macro comma handling, a general diagnostic transaction system, body hashing, binary compatibility shims, broad conformance/vtable identity repairs, and runtime/ABI calling-convention changes.

**Escalate:** A supported template shape cannot be represented without substituting its own parameters, or the fix requires changing declaration equivalence or conformance precedence. Do not substitute source ordinals or hashes to avoid those questions.

## Implementation Plan

1. Add focused regressions for invalid bounded aliases with singleton versus multiple candidates, typed/inferred selectors, and function values. Validate original alias syntax before expansion and integrate the shared validated-list stage. Remove selector resolution from diagnostic-suppressed probing. Preserve existing valid alias/default tests.
2. Implement canonical semantic template descriptors and tests for parameter renaming, alias/default equivalence, unordered bound order, complete spec paths, mixed type/`comp` parameters, and distinction before substitution. Populate checked-function metadata from the driver for free and supported generic method bodies. Keep existing item/cache identity and body materialization unchanged.
3. Extend the standalone mangling model and wire format, then MIR adaptation/lowering under the approved ABI migration. Update encoder/decoder/demangler round trips and migrate generic symbol expectations deliberately. Verify nongeneric symbols and forced names remain byte-identical; rebuild compiler and runtime/package artifacts for further checks.
4. Replace the different-signature workaround in `tests/t05j_generic_bound_selectors` and its provider with actual same-signature A/B/A+B overloads, updating output expectations to identify each body. Add the original zero-argument marker case and function-address checks. Retain the alias regression added during review. Adjust the similarly avoided case in `compiler/omega-driver/tests/generic_methods.rs`.
5. Add a focused CLI integration test file, proposed `compiler/omgc/tests/generic_overload_linkage.rs`, using the process/toolchain conventions in `separate_linkage.rs` and `aligned_separate_linkage.rs`. Compile a provider and independent A/B/A clients, then link and execute them in both object orders. Check correct results, same-declaration address equality, and different-declaration address inequality. Include ordinary generic owner/method coverage and declared-package identity across different physical roots.
6. Update `docs/architecture/symbol-mangling.md` with the exact descriptor grammar/identity, migration, and weak-folding contract; update the relevant metadata/query sections in `semantic-analysis.md` and `module-driver-and-linkage.md`. Clarify in `docs/language/generics.md` that selector validity is caller-owned and candidate-independent, linked to the existing alias-bound rules. No selection-rule redesign is needed. Remove the two resolved review entries from `docs/issues/compiler-limitations.md` only after their reproductions pass, and remove the tests' workaround comments.
7. Run the focused checks below, then the normal workspace/conformance gates with rebuilt artifacts. Review the final diff for missing generic symbol paths, identity depending on local IDs/order, suppressed validation errors, and any unnecessary new comments or duplicate normalization.

## Testing

**Required executable conformance:** `tests/t05j_generic_bound_selectors` must exercise same-name/same-signature declarations at identical concrete generic arguments, typed and inferred A/B/A+B selection, unbounded selection, methods, function values, aliases, and the zero-argument marker example. Compare exact stdout. `tests/t05k_generic_bound_selector_errors` must check the specific failed alias obligation, unknown/non-spec/inaccessible selector names, and no acceptance or diagnostic disappearance when a second overload is added. Add repeated/reordered overload and constrained-conjunction-alias cases; diagnostics should be reported once per invalid written selector. Preserve successful bare versus explicitly applied spec aliases and spec defaults.

**Cross-process regression:** Independently compile clients against one declared provider identity: client A selects the 10-returning declaration, client B selects the 20-returning declaration, and another client selects A using an equivalent spelling. Link both object orders and assert results `10/20`, equal A addresses, and distinct A/B addresses. Each individual compilation must succeed. Inspect names and weak bindings as supporting evidence, not a substitute for execution. Repeat a build with generic parameter renaming, bound order/alias spelling changes, reordered declarations, and an unrelated overload added; unchanged semantic declarations at fixed arguments must keep their names. Use simple C harness conventions already present in CLI integration tests; no new language/runtime dependency is needed.

**Component tests:** New descriptor/adapter cases distinguish symbolic parameter occurrences from coincident concrete types, different fully qualified specs with the same short name, type versus value slots, and all supported nested pattern shapes. The same declaration must normalize identically across defaulted/explicit spec arguments and aliases. Test that function generic defaults do not alter the name when concrete arguments are fixed. Test encoder/decoder round trips, demangled distinction, malformed descriptors, invalid references, and old non-template fixtures. Driver tests must assert no losing function body is materialized and invalid selectors never become successful prepared calls or function values.

**Specification trace:** `docs/language/functions.md` owns overload selection and same-declaration function-address identity; `generics.md` owns selectors/instantiation; `aliases.md` owns transparent aliases and mandatory alias-bound checks. `docs/architecture/symbol-mangling.md` owns deterministic names and weak linkage across independent compilations. The new tests enforce these existing contracts rather than blessing the documented bugs.

Focused commands after the corresponding steps (the new CLI test name is proposed):

```sh
cargo test -p omega-driver --test generic_methods --test aliases --test conform
cargo test -p omega-analyzer -p omega-mangle -p omega-mir
cargo test -p omgc --test generic_overload_linkage --test separate_linkage --test aligned_separate_linkage --test global_mangling_linkage
just build-omgc build-runtime
./bin/test-runner t05j_generic_bound_selectors t05k_generic_bound_selector_errors t05d_generic_overload_ranking t05h_generic_function_values t05i_generic_function_value_errors t10_generics t10c_generic_member_functions t11b_generic_spec_requirements t26_aliases t26b_alias_errors t34_comp_generics t34b_comp_generic_errors
cargo test --workspace
just test-all
```

Use the repository's current Rust/LLVM environment setup. The host separate-process link tests are mandatory for this fix; report unavailable toolchain tests as skipped, never passed. No new cross-target runtime matrix is required because the wire identity is target-independent apart from existing type/value representation and no calling convention/layout is changed. Existing cross-target mangling/type tests must remain passing.
