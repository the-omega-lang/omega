# Inference-hole correctness follow-up

## Task Description

- **Deliverable:** fix the five reproduced review findings in the committed inference-hole implementation, with conformance regressions. The archived plan `docs/plan/0067-infer-syntax.md` is historical and stays unchanged.
- **Purpose:** enforce the existing language rules: forbidden holes produce diagnostics, spec selection has exactly one answer, and known parts of binding annotations guide initialization without speculative instantiations.
- **Chosen direction:** retain `Expected`, `TypePattern`, and single-pass initializer analysis. Give annotation pattern construction an explicit purpose distinct from overload declaration construction; add one-sided pattern-to-pattern inference to the existing overload engine; preserve partial expectations across every `if` branch; identify spec requirements by declaring spec plus instantiated arguments.
- **Rejected alternatives:** probing/rechecking initializers; instantiating candidates to discover their return types; a new inference representation; globally folding overload declaration patterns (these also determine template/symbol identity); changing normative semantics to match the bugs.

## Technical Details

### Initial context boundary

- `compiler/omega-analyzer`: `analysis/items/holes.rs`, `analysis/calls/{pattern,overload,spec}.rs`, `analysis/exprs/mod.rs`, `generics/pattern.rs`.
- Direct consumers: `analysis/items/mod.rs`, `analysis/stmts.rs`, `analysis/literals.rs`, `analysis/expected.rs`. Consult `analysis/specs.rs` for `FlattenedSpecFn::spec_args` and existing requirement identity.
- Normative docs: `docs/language/generics.md` (Inference holes), `control-flow-and-operators.md` (`if`), `specs-and-conformance.md` (Calling conforming functions). Implementation: the expectation and overload sections of `docs/architecture/semantic-analysis.md`.
- Read `docs/guide/quick-reference.md` before writing fixtures and use `docs/architecture/testing-and-validation.md` for test conventions.
- Parser/HIR, driver production code, MIR, codegen, runtime, and mangling remain closed unless a concrete contract problem requires escalation.

### Findings and decided fixes

1. **Forbidden nested holes can silently erase a function.** With an ordinary two-field `Pair<T, U>`, `main() => void { x : Pair<_, *spec _> = 1; }` currently compiles successfully, but emits no `main`. `check_holed_initializer` propagates a silent `None` from `overload_type_pattern`'s unsupported spec-member branch.
   - Validate forbidden holes before rewriting/building an annotation pattern. Walk spec references and anonymous-enum members for all hole spellings (`Type::Infer`, `GenericArg::Infer`, `ArrayLength::Infer`), including nested containers, and report one `InferenceHoleNotAllowed` for the invalid annotation. Do not turn their holes into parameters.
   - Change the preparation contract to distinguish **no inferable holes**, **prepared annotation**, and **diagnosed failure**, e.g. `Result<Option<HoledAnnotation>, ()>`. Update `resolve_typed_decl_init`, `for_in_element_annotation`, and the rewriting tests accordingly. An error must not fall through to the ordinary annotation path or suppress its diagnostic.
   - Annotation-mode construction is diagnostic, unlike speculative overload matching: every failure must record an appropriate error. Inspect its reachable silent exits in `calls/pattern.rs`, including generic-argument kind and function-convention handling. Reuse existing diagnostics and `Context::resolve_convention`; do not resolve synthesized `$Hole` names just to manufacture an error. Distinguish actual resolution failure from unsolved read-back so `UninferredHole` is not appended to an already diagnosed type error.

2. **Distinct generic spec applications are incorrectly deduplicated.** Given `P<T> { f() => Self; }` and implementations of both `P<u8>` and `P<u16>` for `S`, `<S : _>::f()` currently succeeds.
   - In `infer_qualified_spec`, retain the declaring requirement's `spec_id`, `spec_args()`, and name from `FlattenedSpecFn`. Deduplicate by that identity, matching `flatten_spec_into`, rather than `Rc::ptr_eq` alone.
   - Consider every flattened requirement of the requested name, not just `.find(...)`: one refined spec can expose distinct applications of a generic parent. Deduplicate identical inherited applications reached through multiple refinements.
   - Dispatch through the unique declaring spec/application. Ambiguity diagnostics must display concrete generic arguments (`P<u8>`, `P<u16>`) and distinguish qualified names when needed, in deterministic order. Update `QualifiedSpecAmbiguous` in `error/kind.rs` and its renderer as needed; preserve inherent-function exclusion and ordinary visibility/conformance checking.

3. **Overloaded generic calls drop partial expectations.** With `make<T,U>(a:T,b:U) => Pair<T,U>` and an additional one-argument `make<T>`, `x : Pair<_, u8> = make(1, 2)` infers `Pair<i32,i32>` and fails. The complete annotation succeeds. `resolve_overload_candidates` uses only `expected.exact()`.
   - Add a pure operation on `TypePattern` (proposed name `infer_from_pattern`) that seeds a candidate's bindings from an expectation pattern. Its two parameter namespaces are separate: receiver parameters belong to the callee; expectation parameters are binding holes and never bind anything.
   - An expected `Fixed(t)` delegates to existing `infer(t, bindings)`. Recurse through corresponding pointer/slice/array, sized-array, nominal, and function shapes. For nominal types require matching identity and positional argument kinds; known value arguments/array lengths seed callee `comp` parameters. Expected parameter slots contribute nothing. A callee parameter can take a complete known type, never a partially known pattern. Keep existing bindings, widening, and `canonicalize_comp_bindings` behavior.
   - Wire this into candidate seeding after written bindings and before argument inference. Leave exact expectations, argument scoring, bounds, unbounded-position preference, and winner-only instantiation intact. This is inference input, not a new return-type overload ranking rule. Fixed conflicts remain ordinary compatibility errors; do not solve annotation holes during candidate selection.
   - Keep `unify_generic_pattern` for the existing raw-signature call path; the new operation is the counterpart for the overload engine's already-built templates, within the same pattern module. No resolver queries or expression analysis in the pure operation.

4. **Known structured annotation arguments are treated as unknown.** `x : Pair<_, [2]u8> = Pair { a = 1; b = [2, 3]; };` incorrectly infers `[2]i32`. Literal seeding accepts `Fixed`, but the builder leaves the hole-free array structural.
   - Refactor the existing recursive builder in `calls/pattern.rs` to accept an internal construction purpose, with wrappers for overload declarations and binding annotations. Thread that purpose through argument/default construction; do not copy the builder into a second implementation.
   - In annotation mode, after alias expansion, resolve a subtree that contains no synthesized hole parameters normally and represent it as `Fixed`. Determine dependencies across type arguments, array lengths, and nested function/container types. Preserve symbolic subtrees containing holes. Ordinary resolution supplies validation, nominal identity, and alias obligations.
   - Preserve defaults' declaration-module lookup and substitution. A fully known subtree introduced by a default must also become exact after substitution. Do not assume `TypePattern::resolved(&[])` handles every known shape: it deliberately cannot construct nominal or spec-object types.
   - Use the annotation wrapper for initializers and for-in selection. Leave overload-mode structure and template descriptors unchanged. Existing `Expected::narrow` and literal seeding can then consume `Fixed` without local array/pointer special cases.

5. **A diverging first `if` branch discards the annotation pattern.** `x : Pair<_, u8> = if false { return; } else { make(1, 2) };` incorrectly infers `Pair<i32,i32>`; the complete annotation succeeds.
   - In `analyze_if`, pass a surrounding `Expected::Pattern` unchanged to each branch and the final `else`. A diverging branch supplies no replacement expected type. Retain the current `Expected::Exact` and absent-expectation behavior, and retain final branch compatibility checks. Do not introduce backtracking or cross-branch inference.
   - Cover multiple `else if` branches, a non-diverging first branch, and incompatible branch results. `match` already forwards its expectation; use it as a focused regression, not a refactoring target.

### Interfaces and invariants

- The initializer is analyzed once. Read back holes only from its checked type, resolve the completed annotation, and perform ordinary coercion/compatibility checks.
- Binding-hole parameters never escape into checked types, diagnostics, monomorphization keys, or symbol names.
- Annotation validation cannot fail silently, including in for-in preparation. A rejected fixture must fail during compilation with the intended diagnostic, not at link time because a function disappeared.
- Normalizing known annotation subtrees may resolve types that are actually written, but must not instantiate alternate function candidates. Overload template identity, ABI, and symbol spelling remain unchanged.
- **Out of scope:** middle defaulted-hole resolution, new supported hole positions, function-value pattern inference, operators/ranges/`?`, spec-application inference, macro generic-comma parsing, unused-declaration checking, global diagnostic/driver redesign, or a broader `Expected` migration.
- **Stop and escalate:** a required case needs resolver interface changes, new `TypePattern` variants, changed overload preference, or altered template identity. Do not hide a failed acceptance case by adding a normative caveat.

## Implementation Plan

1. Add regression fixtures for all five findings before fixing them. Extend existing `t47a_inference_holes` and `t47b_inference_hole_errors`; isolate the silent-failure case in a new package `tests/t47d_inference_hole_rejection` so unrelated errors cannot conceal compiler success. Its invalid local is unused on purpose.
2. Make annotation preparation explicitly fallible and reject holes in forbidden subtrees. Add diagnostic assertions in `analysis/items/tests.rs`, replacing the current test that merely leaves forbidden holes for later rejection.
3. Add annotation-purpose normalization to the shared pattern builder and route both annotation consumers through it. Verify known arrays and nominal/function/pointer subtrees; keep overload-purpose representations unchanged.
4. Implement and unit-test one-sided template/expectation inference in the existing generics pattern module; wire it into `resolve_overload_candidates`. Verify only the chosen specialization is emitted.
5. Fix `infer_qualified_spec` identity, flattened-candidate enumeration, and diagnostic displays; test both real ambiguity and refinement deduplication.
6. Preserve pattern expectations throughout `analyze_if`, with branch compatibility regressions.
7. Update the expectation paragraph in `docs/architecture/semantic-analysis.md` to explain annotation-only normalization and overload pattern seeding. Clarify in `docs/language/specs-and-conformance.md` that deduplicated declaration identity includes its generic arguments. Other normative rules already describe the required behavior. Do not edit the archived plan or unrelated limitations.
8. Run focused tests, inspect emitted specialization symbols, then run the full gate and report results. Review the final diff against the five findings and the no-extra-instantiation invariant.

## Testing

- **Successful conformance (`t47a`):** known `[2]u8` inside `Pair<_, ...>` with unsuffixed array elements; a known nested nominal type and function type inside a partial annotation; overloaded `make` with an irrelevant-arity overload; overloaded `pair_of<T> => Pair<T,T>` solving the hole as `u8`; known `comp` argument seeding; diverging first and intermediate `if` branches; non-diverging `if` and `match` regressions; refinement diamonds reaching the same generic spec application. Use exact assignments plus observable fields/sizes/output to prove inferred types. Keep complete-annotation counterparts where they clarify parity.
- **Negative conformance (`t47b` and isolated `t47d`):** the exact `Pair<_, *spec _>` silent-failure reproducer; mixed legal and forbidden holes under spec applications/anonymous enums; for-in forbidden holes; ambiguous direct `P<u8>`/`P<u16>` implementations and distinct inherited applications; conflicting known annotation parts and incompatible branch results. Check `expected.stderr` exactly, with one dedicated hole error per invalid annotation and no `$Hole` text. The isolated reproducer must fail at compilation even when its binding is never read.
- **Component tests:** `analysis/items/tests.rs` for preparation outcomes and diagnostic ownership; `generics/tests.rs` for one-sided pattern inference (independent parameter indices, no binding from a hole, repeated callee parameter, written binding precedence, nested structure, nominal mismatch, and `comp` kind/canonicalization); builder tests for annotation-only `Fixed` normalization, including alias/default paths, and unchanged overload-mode patterns. Use existing analyzer test support; no new testing abstraction is required.
- **Specification trace:** `generics.md` Inference holes and inference priority; `functions.md` Overloading; `control-flow-and-operators.md` expected types on every value-producing branch; `specs-and-conformance.md` unique qualified spec selection.
- **Commands:** `cargo test -p omega-analyzer`; `cargo build -p omgc`; with runtime artifacts prepared, `./bin/test-runner t47a_inference_holes t47b_inference_hole_errors t47c_inference_hole_name t47d_inference_hole_rejection t05d_generic_overload_ranking t05e_generic_overload_ambiguity t05j_generic_bound_selectors t05k_generic_bound_selector_errors t11_specs_and_conformance`; then `just test-all` for the normal full gate.
- **Instantiation check:** inspect `target/tests/t47a_inference_holes/objects/` with `nm` after the focused run. Use a uniquely named overloaded regression helper; require one used specialization with `i32,u8`, no unused `i32,i32` specialization, and no losing overload body. Report the result. No separate backend/freestanding matrix is needed because ABI/runtime contracts are unchanged.
