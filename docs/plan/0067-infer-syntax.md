# Inference holes (`_`)

## Task Description

- **Deliverable:** `_` becomes Omega's inference-hole syntax. It is accepted in every syntactic slot the language can infer, it is solved from the surrounding context, and it must resolve to exactly one result. It enables partial, non-prefix inference. The `spec A` generic-argument selector is removed and replaced by `_ : A`. Declaration-level array-length inference `[]T` is removed and replaced by `[_]T`.
- **Purpose:** today a written generic list is a positional prefix, so inference is all-or-nothing per position suffix, and binding annotations must be complete types. `_` lets a program fix exactly the parts it cares about. It does this without a second inference mechanism (Omega invariant: no duplicate mechanisms).
- **Current state (uncommitted working tree):** steps 1–6 below are implemented and pass their focused tests. That covers the lexer/AST/parser, HIR desugaring, the rejection choke point, function generic arguments and selectors, aggregate/owner-path holes, and fully qualified spec calls. The whole-annotation form `x : _ = e` also works. **Not done:** binding-annotation nested holes (step 7, redesigned below), for-in holes, docs (step 8) and the new conformance cases.
  - `compiler/omega-analyzer/src/analysis/items/holes.rs` contains a *probe-then-recheck* solver (`solve_annotation_holes`). It is superseded by this revision and must be replaced. Its hole rewriting (`rewrite_annotation_holes`, `$Hole{n}` params) is reused.
  - `resolve_typed_decl_init` (`analysis/items/mod.rs`) already routes holed annotations there. `Type::InferredArray` and `ArraySizeNotInferable` are already removed. `tests/t02…` and `tests/t08…` already use `[_]i32`, so they fail until step 7 lands.
- **Chosen direction (settled with the user):**
  - `_` is **not** a wildcard. Each `_` is a distinct hole that is solved by the language's existing inference for that position. It is an error when nothing determines it, and an ambiguity when more than one result remains. A `_` position resolves exactly as an omitted position would today, using the `generics.md` priority list, so declared defaults apply (see the default limitation under Risks).
  - A `_` generic argument still counts as **written**. It restricts overload selection to generic declarations, it counts toward arity, and it contributes nothing to the unbounded-position preference (`functions.md` § Overloading).
  - Selector forms on a function generic argument:
    - `M : A + B`: bind `M` and select on the bound set.
    - `_ : A + B`: infer the type and select on the bound set. This replaces `spec A + B`.
    - `M : _`: bind `M` and infer the bound set. It is **not** a plain type argument for the preference rule, so with `f<T>` and `f<T: A>` both applicable, `f<M>` picks `f<T>` but `f<M : _>` is ambiguous.
    - `_ : _` means the same as `_`.
  - Fully qualified spec calls `<S : P>::f(...)` have two hole slots:
    - `<_ : P>::f(...)` means `P::f(...)`: `Self` comes from the receiver, or for a receiverless function from the expected type.
    - `<S : _>::f(...)` infers the unique spec among `S`'s conformances that declares `f`. Specs reaching the same declaring spec through refinement count once, and inherent functions are not candidates. No candidate is an error; more than one is ambiguous.
    - `<_ : _>::f(...)` is rejected.
  - `x : _ = e` is equivalent by construction to `x := e` (HIR walrus desugar).
  - **Nested holes in binding annotations are solved top-down** (redesigned; replaces the probe). The annotation, with holes as parameters, becomes an *expectation pattern* that flows into the initializer the way a complete annotation's type does, so `a : u64 = 10` and `a : Pair<_, u8> = …` work the same way. Sites that already choose from the expected type read the pattern's known parts. After the single analysis, each hole is read back from the checked initializer type.
    - Examples that must hold:
      - `q : Pair<_, u8> = Pair { a = 1; b = 2; }` gives `b : u8`.
      - `a : [_]u8 = [1, 2, 3]` gives `[3]u8`.
      - `m : Pair<_, u8> = make(1, 2)` instantiates only `make<i32, u8>`.
      - `x : Pair<_, u8> = pair_of(1)` with `pair_of<T>(v: T) => Pair<T, T>` solves `_ = u8`.
  - `_` is accepted **only** where omitting the thing is already inferred. Everywhere else it gets `InferenceHoleNotAllowed`, never "unknown type `_`". The rejected places are:
    - parameter, return and field types;
    - generic defaults and alias right-hand sides;
    - bounds and spec bodies;
    - casts;
    - `spec _` and `*spec _`;
    - `x : _;` without an initializer;
    - array lengths outside an inferable binding;
    - holes inside a spec application;
    - holes inside an anonymous-enum member of an annotation.
  - `_` is a reserved token and never an identifier.
- **Rejected alternatives:**
  - *Keeping `spec A` or `[]T` inference beside the new spellings:* duplicate mechanisms.
  - *Probe-then-recheck for binding holes* (the previous revision):
    - it materializes generic instantiations that are never used (`m : Pair<_, u8> = make(1, 2)` emitted `make<i32, i32>`), and the analyzer has no rollback;
    - it solves holes without the annotation's known parts, so the `pair_of` example is wrongly rejected.
  - *A `ResolvedType::Infer` variant:* it could leak into checked types, and consumers would tolerate it silently. The new expectation type forces the compiler to list every consumer instead.
  - *A new pattern representation:* `generics::pattern::TypePattern` already represents "a resolved type with parameter placeholders". The overload engine builds it (`overload_type_pattern`) and infers from it (`TypePattern::infer`). Holes are its parameters.
  - *Treating `M : _` as plain `M`:* redundant, and it would let the preference rule guess.

## Technical Details

- **Initial context boundary:**
  - `compiler/omega-analyzer`, specifically:
    - `analysis/items/{mod,holes}.rs`;
    - `analysis/exprs/mod.rs` (`analyze_expr` and its pass-through sites);
    - `analysis/literals.rs`;
    - `analysis/calls/{generic,pattern}.rs`;
    - `analysis/stmts.rs`;
    - `generics.rs` and `generics/pattern.rs`.
  - Docs as listed in step 8.
  - Parser, HIR, driver, MIR and codegen stay closed; steps 1–6 already did what they need. Globals with an initializer go through `resolve_typed_decl_init`, so they need no driver change.
- **Affected files/symbols (remaining work):**
  - **New `Expected<'a>` type** in a new `analysis/expected.rs`:
    - Shape: a `Copy` enum `{ None, Exact(&'a ResolvedType), Pattern(&'a TypePattern) }`.
    - Accessors:
      - `exact() -> Option<&ResolvedType>`;
      - `From<Option<&ResolvedType>>`;
      - a narrowing helper that turns a sub-pattern into an expectation: `Fixed(t)` becomes `Exact(t)`, a hole `Parameter` becomes `None`, and anything else stays a `Pattern`.
    - It replaces every `expected: Option<&ResolvedType>` parameter in the analyzer's expression analysis (currently 57 signatures across `analysis/`) and the `analyze_expr` argument (55 call sites).
    - `coerce_to_expected` keeps taking a resolved `Option<&ResolvedType>`: it runs only once a complete type is known.
  - **Pattern-aware consumers.** Every other consumer uses `.exact()`, which treats a pattern as no expectation.
    - `analyze_array_literal` (`literals.rs:~1030`): a `SizedArray`/`Array`/`Slice` pattern gives each element the narrowed item expectation. The length stays solved by the literal.
    - `analyze_address_of` / `analyze_address_of_inner` / `analyze_const_slice` (`exprs/mod.rs:~609`): a `Pointer`/`Slice` pattern passes the narrowed pointee to the operand.
    - Pass-through that must forward the `Expected` unchanged:
      - block tail (`analyze_block`);
      - `if` and `match` arms;
      - `analyze_comp`, `analyze_reveal`.
    - Struct/enum literal inference: `infer_literal_type_args` / `expected_matches_generic_item` (`literals.rs`). A `Nominal(path, args)` pattern naming the literal's item seeds each `ArgumentPattern::Type(Fixed(t))` and each `Value(v)` into the existing `seed` parameter (added in step 5). `Parameter` and structured positions stay open for the fields.
    - Generic calls: `seed_from_expected` (`calls/generic.rs`; callers at lines ~116, ~592, ~786). With a `Pattern` it calls a new `unify_generic_pattern(generics, raw, &TypePattern, subst)` in `generics.rs`, placed beside `unify_generic_type` and mirroring its arms:
      - `Fixed(c)` delegates to `unify_generic_type`;
      - a hole `Parameter` binds nothing;
      - `Pointer`, `SizedArray` (value length binds a raw `comp` length), `Nominal` (positional over args, same kind rules as the `Type::Generic` arm) and `Function` recurse.
    - That one-sided "binds callee parameters only from known parts" rule is the two-sided unification. Holes themselves are solved later by read-back, never here.
  - **`analysis/items/holes.rs`:**
    - Keep `rewrite_annotation_holes`. Switch its `GenericArg::Infer` kind lookup to the identity the pattern builder uses: `pattern_item_path` + `resolver.item_generic_params`, instead of `generic_prefix_absolute`.
    - Replace `solve_annotation_holes` with the algorithm under Interfaces.
    - Make `overload_type_pattern` and `pattern_item_path` (`calls/pattern.rs`) `pub(crate)`, or otherwise reachable from `items`.
  - **`analysis/stmts.rs`, `classify_for_in_source` (~line 489):**
    - A binding type containing holes is rewritten and built into a pattern.
    - A `ToIterator` candidate item type matches when `TypePattern::infer` solves every hole and `TypePattern::exact(candidate, &bindings)` holds.
    - More than one match is `ForInIteratorAmbiguous`-style ambiguity, reusing the existing diagnostic if one fits, otherwise a new one. Zero matches keeps the current error.
    - The for-in binding is then typed by the solved type.
  - **`docs/issues/language-limitations.md`:** add a `## Generics` section recording the default-position limitation (see Risks).
- **Interfaces/invariants:**
  - **Solving holes in a binding annotation:**
    1. `rewrite_annotation_holes` produces the rewritten annotation plus hole params `$Hole{n}` (type holes, `comp usize` for array lengths, the declared kind for `GenericArg::Infer`).
    2. `overload_type_pattern(decl_id, decl_span, &rewritten, &holes)` builds the pattern. Holes become `Parameter(i)` or `ArgumentPattern::Parameter(i, _)`. Hole-free subtrees are resolved `Fixed`, reported as a written annotation would report them, and declared defaults are appended by `overload_argument_patterns`.
    3. `analyze_expr(value, Expected::Pattern(&pattern))`: exactly **one** analysis of the initializer.
    4. `bindings = vec![None; holes.len()]`, then `pattern.infer(&checked.r#type, &mut bindings)`. A hole still `None` is `UninferredHole` at the declaration (e.g. `x : Pair<_, u8> = 5`).
    5. Resolve the rewritten annotation under the solved bindings with `with_substitution` + `resolve_type_or_error(.., indirect: true)`, which gives alias-bound obligations and full-type diagnostics. Then run the existing tail unchanged: `coerce_to_expected`, then `value_type_compatible` / `AssignmentTypeMismatch`.
    - A conflict such as `x : Pair<_, u8> = Pair<i32, i32> { … }` therefore reports exactly what `Pair<i32, u8>` would report.
  - A `Parameter` inside an `Expected::Pattern` always denotes a binding hole, never a callee parameter. Callee templates' own `TypePattern`s never flow through `Expected`.
  - No site may instantiate, register or emit anything based on a `Pattern` expectation other than what the single analysis already does with the known parts. After step 7, `m : Pair<_, u8> = make(1, 2)` must produce exactly one `make` instantiation.
  - `$Hole{n}` names never appear in user-facing text. Diagnostics display the written annotation (`HoledAnnotation::written`).
  - Monomorphization identity, mangling and ABI are unaffected: holes are solved before any instantiation exists.
- **Out of scope:**
  - Holes in spec applications (`Spec<_>::f`, `<S : Spec<_>>::f`): rejected, recorded under Specs in `language-limitations.md`.
  - Holes in casts, patterns, and `comp` typed declarations.
  - Pattern expectations for sites other than those listed: non-generic call arguments, operators, ranges, `?`. They treat a `Pattern` as no expectation.
  - Renaming `Type::InferredArray`.
  - Resolving a defaulted hole in the declaration context (see Risks).
  - The pre-existing inability of `println$`'s `expr...` splitter to accept a `<a, b>` generic list inside an argument. Tests bind such calls to locals first.
- **Risks/open questions (stop and escalate):**
  - **Default-position limitation, keep as documented debt:** `resolve_inferred_generic_args` (`generics.rs`) now reports an unbound *defaulted* parameter that is followed by a bound one as undetermined, instead of silently truncating the list and dropping the later binding, which `mid<_, u8>()` with `mid<T = u16, U = u32>` used to do.
    - A proper fix resolves the default in the declaration's own module (the driver's `complete_overload_arguments`), which crosses the resolver interface.
    - Record it in `docs/issues/language-limitations.md`; do not fix it here.
    - The conformance "declared default" case must use a trailing `_` (e.g. `defaulted<_, _>(1)`).
  - If migrating to `Expected` shows a consumer that *needs* pattern information beyond the list above for a test case in this plan to pass, stop and report it rather than adding ad-hoc pattern handling.
  - If `overload_type_pattern` cannot represent an annotation shape that the plan's tests require (it returns `None` for unrepresentable shapes), escalate rather than extending `TypePattern`.

## Implementation Plan

Steps 1–6 are done (see Current state). Remaining:

7a. **`Expected` migration (pure refactor).**
   - Introduce `analysis/expected.rs`.
   - Replace `Option<&ResolvedType>` expectation parameters in expression analysis with `Expected<'_>`. Consumers call `.exact()`; callers wrap with `.into()` / `Expected::Exact`.
   - No `Pattern` is constructed yet.
   - `cargo test -p omega-analyzer` and the full conformance suite must be unchanged, except `t02`/`t08`, which await 7c.

7b. **Pattern consumers.**
   - Add `unify_generic_pattern` and wire `seed_from_expected`.
   - Add pattern handling in the array-literal, address-of/const-slice, struct/enum-literal seeding and pass-through sites listed above.

7c. **Binding-annotation solver.**
   - Replace `solve_annotation_holes` with the Interfaces algorithm and delete the probe code.
   - Expose `overload_type_pattern` / `pattern_item_path` as needed, and switch hole-kind lookup.
   - `t02`/`t08` pass again.

7d. **For-in holes** in `classify_for_in_source` via `TypePattern::infer` + `exact`.

8. **Docs.**
   - Add a normative subsection "Inference holes" to `docs/language/generics.md`. It covers:
     - the principle and per-position meaning;
     - defaults, arity and preference;
     - top-down solving of annotation holes from their known parts.
   - Link to it from `bindings-and-mutability.md` (annotations) and `strings-casts-arrays-and-slices.md` (`[_]T`).
   - Replace the `spec A + B` selector text in `generics.md` § Bound selectors and in `functions.md` § Overloading / function values with `_ : A + B`, and add the `M : _` rule.
   - `grammar.md`: extend the `type`, `generic-argument`, `array-length` and `expression-generic-argument` productions.
   - `lexical-structure.md`: `_` is a reserved token.
   - `specs-and-conformance.md` § Calling conforming functions: `<_ : P>` and `<S : _>` meanings, and the `<_ : _>` rejection.
   - Update the `[]i32 =` examples in `types-and-primitives.md` and `strings-casts-arrays-and-slices.md`.
   - `docs/guide/quick-reference.md`: `_` holes, and the `pick<_ : B>` selector replacing `pick<spec B>`.
   - `docs/architecture/semantic-analysis.md`: one short paragraph on `Expected` (exact vs pattern) and the rule that only choosing sites read patterns.
   - `docs/issues/language-limitations.md`: spec-application holes (Specs) and the default-position limitation (new Generics section).
   - Do not edit `docs/plan/`.

## Testing

- **New conformance cases (root `tests/`):**
  - `t47a_inference_holes` (`expected.stdout`). Print values or `sizeof` so solved types are observable. It covers:
    - `x : _ = 123` behaving as `:=`;
    - `some_fn<_>(10)` selecting the generic over the concrete overload;
    - `f<_, u32>("hello", 123)` and `f<_, u64, _>("hello", 123, 456)` across 2- and 3-parameter overloads;
    - `thing<i32 : _, _ : SpecThree>(1, 2)` and `thing<_, _ : SpecThree>(1, 2)`;
    - `count<_, i32>(values)` (`comp` position);
    - a trailing `_` taking a declared default;
    - `Pair<_, u8> { a = 1; b = 2; }`, `Pair<_, u8>::new(..)` and `n : Option<u16> = Option<_>::None`;
    - binding holes:
      - `p : *_ = &v`;
      - `q : Pair<_, u8> = Pair { a = 1; b = 2; }`;
      - `a : [_]u8 = [1, 2, 3]`;
      - `b : Buffer<_, i32> = Buffer { data = [4, 5, 6]; }`;
      - `m : Pair<_, u8> = make(1, 2)`;
      - `x : Pair<_, u8> = pair_of(1)` (the solved `_` is `u8`: print `sizeof`);
      - a global `g : [_]u8 = [1, 2]`;
    - for-in `for x : _ in ..` and a nested-hole for-in binding;
    - `_ : A` in function-value selection;
    - `x : Widget = <_ : Make>::make()`, `<_ : Animal>::make_sound(&dog)` and `<Widget : _>::make()`.
  - `t47b_inference_hole_errors` (`expected.stderr`). Each case asserts its diagnostic:
    - `x : _;`;
    - `_` in a parameter type, return type, field, alias right-hand side (used), generic default, cast and spec application;
    - `spec _`;
    - undetermined `f<_>()`;
    - `x : Pair<_, u8> = 5` (`UninferredHole`, showing the written annotation, not `$Hole`);
    - `x : Pair<_, u8> = Pair<i32, i32> { … }` (mismatch against `Pair<i32, u8>`);
    - `f<M : _>` ambiguous;
    - `f<spec A>` (migration diagnostic);
    - `v : []i32 = [..]` (`BareUnsizedArray` with the `[_]` hint);
    - `_` as a binding name;
    - `<_ : _>::make()`;
    - `<S : _>::f()` ambiguous, naming both specs;
    - `<S : _>::f()` with only an inherent `f`;
    - the defaulted-position limitation (`mid<_, u8>()`), with its current diagnostic.
- **Specification trace:**
  - the new "Inference holes" subsection of `generics.md`;
  - `generics.md` § Bound selectors;
  - `functions.md` § Overloading (preference);
  - `specs-and-conformance.md` § Calling conforming functions;
  - `strings-casts-arrays-and-slices.md` (`[_]T`).
- **Component tests:**
  - analyzer unit tests (`analysis/items/tests.rs` or `analysis/tests.rs`, where binding resolution is already tested) for hole rewriting/kinds;
  - `unify_generic_pattern` (known parts bind, hole positions do not);
  - the pattern read-back.
  - Parser and HIR tests already exist from steps 1–2.
- **No-extra-instantiation check:** after building `t47a`, `nm` its objects and confirm there is exactly one `make` symbol. State the result in the report; this is not automated.
- **Regression coverage:**
  - `t05*` (especially `t05d`, `t05e`, `t05h`/`t05i`, `t05j`/`t05k`);
  - `t10*`, `t11_specs_and_conformance`, `t02`, `t08`, `t09_iteration_and_ranges`, `t34_comp_generics`;
  - the full suite after 7a, since it touches every expectation site.
- **Commands:**
  - `cargo test -p omega-parser -p omega-hir -p omega-analyzer`;
  - `./bin/test-runner t47a_inference_holes t47b_inference_hole_errors t05j_generic_bound_selectors t05k_generic_bound_selector_errors t02_types_and_primitives t08_strings_casts_arrays_and_slices`;
  - `just test-all`.

  No backend- or target-specific verification is needed, since holes never reach MIR.
