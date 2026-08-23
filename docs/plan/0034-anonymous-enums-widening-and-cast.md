# Anonymous-Enum Widening and Explicit Cast Conversion

## Task Description
- **Deliverable:** Extend the existing anonymous-enum conversion machinery so both expected-type coercion and explicit casts can convert compatible values into an explicitly established anonymous enum. A value is compatible when every possible canonical source member is present in the destination anonymous enum.
- **Purpose:** Make anonymous enums compose naturally in ordinary typed code and in explicit casts. In particular, `enum A | B` must be usable where `enum A | B | C` is explicitly expected, and `<enum A | B>value` must be able to inject a compatible raw value into that explicitly named anonymous enum. This preserves the language rule that anonymous enums are never synthesized by inference.
- **Chosen direction:** Treat a plain non-anonymous type as a singleton source set and an unrefined anonymous enum as its canonical member set. Conversion to an explicitly established anonymous-enum destination is allowed when the source set is a subset of the destination's canonical members. The destination may be established by an expected type (assignment, return, argument, field, typed `if`/`match`, etc.) or directly by cast syntax (`<enum ...>expr`). A refined anonymous-enum read continues to act as its proven leaf for conversion purposes. Anonymous-enum widening is a real representation-changing conversion and may retag/repack at runtime; it is never modeled as type equality or a bitcast.
- **Examples that must work:**
  ```omega
  value : enum A | B = A{};

  small : enum A | B = ...;
  large : enum A | B | C = small;

  cast_member := <enum i32 | *str>10;
  cast_large := <enum A | B | C>small;
  ```
- **Examples that remain invalid:**
  ```omega
  # No anonymous enum may be synthesized by inference.
  x := if cond { A{} } else { B{} };

  # Narrowing / partial-overlap casts are not introduced here.
  abc : enum A | B | C = ...;
  ab := <enum A | B>abc;

  ab2 : enum A | B = ...;
  ac := <enum A | C>ab2;
  ```
- **Rejected alternatives:** Do not put subset relation into `ResolvedType::accepts`, because widening can change representation. Do not create a separate cast-only anonymous-enum mechanism. Do not infer/join anonymous enums from unrelated expressions. Do not implement runtime remapping only in LLVM; checked IR must record the conversion and MIR must expose the control flow using existing enum primitives.

## Technical Details
- **Current architecture to preserve:** `Analyzer::coerce_to_expected` in `compiler/omega-analyzer/src/analysis/mod.rs` already owns exact anonymous-enum member injection and refined-member projection. `Analyzer::conversion_cost` mirrors those coercions for overload ranking. Explicit casts enter through `Analyzer::analyze_cast` in `compiler/omega-analyzer/src/analysis/exprs/operators.rs`, which currently resolves the target and then proceeds through spec/pointer/scalar cast classification. Runtime enum construction already lowers through shared enum MIR/codegen machinery.
- **Core semantic rule:**
  - A destination anonymous enum must already exist explicitly in semantic context. Expected-type contexts and cast syntax are both valid ways to establish it.
  - A non-anonymous source has one possible type: itself (respecting the existing refined/widened nominal-type handling used by anonymous member injection).
  - An unrefined anonymous-enum source has the canonical members of its shape as its possible types.
  - Conversion exists iff all source possibilities occur in the destination's canonical shape.
  - Equality/representation-preserving acceptance is handled before conversion.
  - Destination-to-smaller narrowing and partial-overlap conversion remain invalid.
  - This conversion rule does not synthesize a new anonymous-enum type and therefore does not weaken the explicit-only inference rule.
- **Affected files/symbols:**
  - `compiler/omega-analyzer/src/resolved_type.rs` — `ResolvedAnonymousEnum`: add one canonical subset/remap query over already-normalized member lists. It returns the destination canonical index for every source canonical index only if the source is a subset. Keep canonicalization, flattening, type identity, tags, and ordering unchanged.
  - `compiler/omega-analyzer/src/analysis/mod.rs` — refactor `project_refined_anonymous`, `inject_anonymous_member`, and the new subset-widening check behind one shared anonymous-enum conversion helper (name may follow local conventions, e.g. `try_convert_to_anonymous_enum`). `coerce_to_expected` calls this helper after representation-preserving acceptance. `conversion_cost` must use the same compatibility predicate so overload viability cannot disagree with real coercion.
  - `compiler/omega-analyzer/src/analysis/exprs/operators.rs` — `Analyzer::analyze_cast`: after resolving/analyzing the cast target/base and before ordinary cast-class resolution, detect an unrefined anonymous-enum target and invoke the shared anonymous-enum conversion helper. If it succeeds, return that converted checked expression with the cast target type. If it fails, report `InvalidCast { from, to }`; do not fall through to scalar/pointer cast classification for anonymous-enum targets.
  - `compiler/omega-analyzer/src/checked.rs` — add a checked anonymous-enum widening expression carrying the source expression and analyzer-decided source-index -> destination-index remap. Exact raw-member injection can continue using `CheckedExpr::EnumConstruct`; only multi-member runtime widening needs the new checked operation. Both implicit and cast-triggered widening must produce the same checked node.
  - `compiler/omega-analyzer/src/comp_eval.rs` — evaluate the widening node by evaluating the source enum, mapping its active source variant through the stored remap, preserving the active member payload, and rebuilding the destination `ConstValue::Enum`. Raw-member constant injection should continue folding to `ConstValue::Enum` as today, including when initiated by a cast.
  - `compiler/omega-analyzer/src/dead_code.rs` — recurse through the widening source so usage accounting is unchanged.
  - `compiler/omega-mir/src/lower/function/expr.rs` and `control_flow.rs` — lower the checked widening into ordinary MIR: evaluate the source exactly once, inspect the tag, project the active member with `EnumBody`, construct the corresponding destination variant with `EnumConstruct`, store into a destination local, and merge. Use the analyzer-provided remap; MIR must not recompute compatibility/canonical indices.
  - `compiler/omega-mir/src/lower/function/defer.rs` — recurse through the widening source for defer-body discovery.
  - `compiler/omega-analyzer/src/error/render.rs` — expected-type mismatch notes for anonymous enums must describe the subset rule rather than the obsolete blanket claim that Omega has no implicit conversions. Cast failures continue to use the cast diagnostic path (`cannot cast ...`) and should not masquerade as expected-type mismatches.
  - `docs/language/enums-and-pattern-matching.md` — replace the old no-subset/superset statement with the explicit-destination conversion rule. State clearly that raw-member injection and anonymous-enum subset widening are two cases of conversion into an already established anonymous enum, and that inference still never synthesizes one.
  - `docs/language/strings-casts-arrays-and-slices.md` — extend **Explicit casts** with anonymous-enum casts: `<enum A | B>A{}` and `<enum A | B | C>small_ab` are valid when the source possibilities are a subset of the target members; narrowing/partial overlap is rejected. Reuse the existing `<Type>expr` syntax; no grammar change is required.
  - `docs/architecture/semantic-analysis.md` — record ownership: `accepts` is representation-preserving; shared anonymous-enum conversion analysis owns exact injection/subset conversion; expected-type coercion and cast analysis are two callers; analyzer-decided remaps are carried into checked IR and expanded by MIR.
- **Shared helper contract:**
  - The helper receives a fully resolved anonymous-enum target and a checked source expression; it never resolves/invents the target.
  - It first preserves the existing refined-read behavior: if the source is a refined anonymous enum and the target wants the parent type exactly, do not project/repack; otherwise a proven leaf may be projected and injected into any compatible anonymous-enum target.
  - Exact member injection produces the existing `EnumConstruct`/constant representation.
  - Unrefined anonymous-enum subset conversion produces identity when source and destination are already representation-equivalent, otherwise the widening checked node with a stable remap.
  - Arbitrary conversion chains are not searched. For example, if `i16` can cast to `i32`, that does not make `i16` implicitly/cast-directly compatible with `enum i32 | A` through a hidden `i16 -> i32 -> enum ...` chain. The source must itself be a canonical destination member (or anonymous-enum subset) under the existing exact-member rules.
- **Cast-specific behavior:**
  - Cast syntax is an explicit type position and therefore satisfies the "anonymous enum must be explicit" rule even when the surrounding expression is otherwise untyped: `x := <enum A | B>A{};` is valid and `x` simply has the explicitly named anonymous-enum type.
  - `<enum A | B | C>small_ab` uses exactly the same widening checked node and runtime remap as `large : enum A | B | C = small_ab`.
  - `<enum A | B>A{}` uses exactly the same member-injection construction as assignment/return coercion.
  - Casting an anonymous enum to an equal canonical anonymous enum is representation-preserving; preserve existing no-op-cast warning conventions if `analyze_cast` normally warns for identity casts.
  - This task does **not** add anonymous-enum narrowing, runtime checked downcasts, member extraction casts, or partial-overlap conversion.
- **Runtime/ABI invariant:** widening evaluates the source expression once and reconstructs the destination representation. Never assume source tags equal destination tags, even if one spelling visually appears to append members. Canonical sorting may insert destination members before source members, and payload layout/alignment may differ.
- **Out of scope:** no parser/grammar changes; no anonymous-enum inference; no narrowing/downcast semantics; no arbitrary member coercion chains; no layout/tag/canonical-order changes; no new named-enum widening behavior; no LLVM-only conversion primitive; no new cast syntax (the existing `<Type>expr` syntax is reused).
- **Risks/open questions:** none requiring a new language decision. If an expected-type site bypasses `coerce_to_expected`, route it through the shared conversion path. If cast analysis has source-ID/span conventions that require preserving the cast node identity, adapt the shared helper to accept/result-wrap IDs rather than duplicating conversion semantics.

## Implementation Plan
1. **Add canonical subset/remap support.** In `ResolvedAnonymousEnum`, add and unit-test a deterministic inclusion/remap helper that maps every source canonical member index to its destination canonical member index only when the source is a subset. Use canonical `ResolvedType` equality and already-flattened shapes; do not normalize or sort again.
2. **Introduce checked runtime widening.** Add `CheckedExpr::AnonymousEnumWiden` (or local-equivalent naming) with source + resolved variant map. Update checked-expression walkers such as dead-code analysis and MIR defer collection to recurse through it.
3. **Centralize anonymous-enum conversion analysis.** Refactor the existing refined projection and exact member injection path into one analyzer helper for "convert this checked value to this already-resolved anonymous-enum target." It must cover: representation-preserving identity/parent use, refined-leaf projection, exact member injection, constant injection, and unrefined subset widening. It returns failure without emitting a context-specific diagnostic so callers can choose mismatch vs cast errors.
4. **Use the helper from expected-type coercion.** `Analyzer::coerce_to_expected` keeps `target.accepts` as the zero-cost first check, then calls the shared anonymous-enum conversion helper. Add subset widening there without changing inference/join behavior. Extend `conversion_cost` using the same compatibility predicate; exact acceptance remains cost `0`, anonymous-enum construction/widening remains a nonzero conversion cost so exact overloads win.
5. **Use the same helper from casts.** In `Analyzer::analyze_cast`, after target resolution/base analysis and before ordinary spec/pointer/scalar cast classification, special-case an unrefined anonymous-enum target. Attempt the shared conversion. On success, return the converted checked expression. On failure, emit `AnalysisErrorKind::InvalidCast { from, to }` and stop. Do not add a new `CastKind` for anonymous enums: the semantic result is the same checked construction/widening IR used by implicit conversion, not an independently lowered cast operation.
6. **Implement compile-time widening.** In `comp_eval`, evaluate `AnonymousEnumWiden` by selecting the active source variant, applying the stored remap, and rebuilding the destination enum constant. Verify cast-triggered constant injection/widening follows the same path as expected-type conversion.
7. **Lower runtime widening through existing MIR enum primitives.** Evaluate the source once, branch on its tag, project the corresponding payload, construct the mapped destination variant, and merge. Reuse `EnumTag`, `EnumBody`, and `EnumConstruct`; impossible tags end in `Unreachable`. Do not add an LLVM representation-level widening special case.
8. **Fix diagnostics.** Expected-type mismatches involving anonymous enums should explain that implicit conversion requires every possible source member to exist in the destination. Explicit cast failures should remain `InvalidCast` diagnostics and, where useful, note the same subset criterion without using expected-type wording.
9. **Update language/architecture docs.** Document the single conversion rule in the enum docs, then add anonymous-enum examples to the existing cast docs. Explicitly contrast `x := if ... A ... B ...` (still an inference error) with `x := <enum A | B>...` or `x : enum A | B = ...` (destination explicitly established). Preserve the rule that casts do not add narrowing/downcast semantics.
10. **Update conformance tests.** Move the prior widening-rejected assertion to positive widening coverage; retain genuine narrowing/partial-overlap failures. Add cast tests to the same `t27*` feature family rather than creating a separate cast feature package unless test-harness phase separation requires it.

## Testing
- **Analyzer unit tests:**
  - `compiler/omega-analyzer/src/resolved_type/tests.rs`: subset/remap succeeds across canonical/reordered spellings, maps correctly when a new destination member sorts before existing source members, and rejects missing members/partial overlap.
  - Add focused analyzer tests for the shared conversion helper if there is an established local unit-test seam: exact raw-member injection, anonymous-enum widening, refined-member conversion, and failure without mutation of the source expression.
- **Positive source tests (`tests/t27_anonymous_enums`):**
  - Existing raw injection remains valid: `x : enum A | B = A{};`.
  - Implicit widening: `small : enum A | B` assigned/passed/returned/field-initialized where `enum A | B | C` is explicitly expected.
  - **Raw cast injection:** `x := <enum i32 | *str>10;` and/or marker equivalents; inspect/match the result to prove the correct tag/payload.
  - **Anonymous-enum cast widening:** `small : enum A | B = ...; large := <enum A | B | C>small;`.
  - **Runtime retagging regression for both implicit and cast paths:** choose canonical names/types so the added destination member sorts before at least one source member, then match/use the widened value. A naive bitcopy/preserve-tag implementation must fail this test.
  - **Compile-time cast coverage:** a `comp` value/member cast into an anonymous enum and a `comp` smaller-enum cast into a larger enum, proving `ConstValue::Enum` remapping is shared.
  - **Explicitness rule:** `x := <enum A | B>A{};` compiles despite having no surrounding expected type because the cast itself explicitly establishes the target.
  - **Overload ranking:** exact `enum A | B` parameter beats a candidate requiring widening to `enum A | B | C`; existing raw-member-to-enum conversion ranking remains stable.
  - Alias spelling should work identically: casting to an alias whose resolved type is an anonymous enum uses the same conversion rule.
- **Negative source tests (`tests/t27b_anonymous_enum_errors` unless cast diagnostics belong in an existing cast-error package):**
  - `enum A | B | C -> enum A | B` remains rejected as an implicit expected-type conversion.
  - `<enum A | B>abc` where `abc : enum A | B | C` is rejected as `InvalidCast`.
  - `<enum A | C>ab` where `ab : enum A | B` is rejected as partial overlap.
  - `<enum A | B>C{}` is rejected when `C` is not a destination member.
  - Untyped `if`/`match` with `A` and `B` branches remains an error; adding cast support must not create anonymous-enum synthesis/join inference.
  - If existing cast tests assert diagnostic wording/categories, add anonymous-enum cast cases there as well to ensure failures are classified as casts, not field/assignment mismatches.
- **Specification trace:**
  - `docs/language/enums-and-pattern-matching.md`: proves "explicit destination + coherent source => implicit anonymous-enum conversion" and "anonymous enums are never inferred."
  - `docs/language/strings-casts-arrays-and-slices.md`: proves `<AnonymousEnum>expr` establishes an explicit destination and uses the same subset rule, with narrowing explicitly excluded.
- **Regression coverage:** retain `t27c_anonymous_enum_boundary_errors`; anonymous-enum widening/casts must not loosen foreign-boundary or conform-target restrictions. Keep named-enum tests because runtime widening reuses `EnumBody`/`EnumConstruct`, including the previously fixed register-held `EnumBody` spill path.
- **Commands/target coverage:** run `cargo test -p omega-analyzer`, `cargo test -p omega-mir`, focused `t27_anonymous_enums`, `t27b_anonymous_enum_errors`, and `t27c_anonymous_enum_boundary_errors`, plus any existing cast conformance package touched by diagnostics; finish with `just test-all`. No backend-specific widening primitive should exist, so ordinary LLVM verification/run coverage is sufficient once MIR lowering is exercised.
