# Spec and conformance fixes: goal-directed solving, alias transparency, return-type inference

## Task Description

- **What is being asked:** close the open spec/conformance entries in
  `docs/14-known-issues.md`, in six parts:

  1. **Goal-directed conformance solving.** A chain of two blanket
     derivations is misreported as `ConformanceCycle`. Fix the cause, not
     the guard.
  2. **Spec-alias transparency.** `spec AB = A + B` must make `T: AB` and
     `T: A + B` interchangeable, in blanket precedence *and* in bound-context
     entailment.
  3. **Return-type-driven generic inference.** A generic function whose only
     type parameter appears in its return type cannot be called; nor can a
     generic type's static function be resolved from an expected type.
  4. **Remove definition-site `spec T` return types.** `make() => spec Animal
     { Dog{} }` on an ordinary function or method becomes an error. The
     parameter position and the spec-declaration position both stay.
  5. **Two diagnostics that name constructs the author never wrote** — a
     `*mut self` requirement against an rvalue receiver, and a generic
     parameter that could not be inferred from a slice argument.
  6. **Doc corrections** for entries that are stale, misfiled, or that name
     the wrong function.

- **Purpose:** every one of these is a place where Omega's spec system either
  produces a *wrong* answer that depends on declaration order, or tells the
  author something untrue about their own code. Both are the kind of wart
  that an ecosystem grows around. Nothing here changes the ABI, adds runtime
  support, or costs code size; parts 1 and 4 make the compiler *smaller* and
  more phase-honest.

- **Reasoning:**

  **Part 1** is the deep one. `docs/14` calls the cycle guard "deliberately
  conservative". It is not conservative — it is **order-dependent**, which is
  strictly worse. The same three declarations compile or fail depending only
  on the order they are written in (verified against the current tree):

  ```
  conform S to A { … }
  conform<T: A> T to B { … }      # B before C  -> error: cyclic conformance while proving 'S: B'
  conform<T: B> T to C { … }
  ```
  ```
  conform S to A { … }
  conform<T: B> T to C { … }      # C before B  -> compiles cleanly
  conform<T: A> T to B { … }
  ```

  The cause is `Driver::materialize` (`conformances.rs:1045`). It records a
  target as materialized only *after* its template sweep finishes, so a bound
  check firing mid-sweep re-enters the sweep and starts instantiating a
  *second* template while the first is still in flight. That second template's
  bound then asks for the first, which is legitimately in progress, and the
  `(target, spec)` guard in `conformance_for` can only call that a cycle. The
  guard is a symptom; the sweep is the bug.

  The fix is to make proving **goal-directed**: `conformance_for(target, spec)`
  instantiates only the templates that can produce *that spec*, never every
  template on the type. A goal stack keyed on `(target, spec)` then detects
  cycles exactly — a cycle is when the stack closes on itself — and the answer
  stops depending on declaration order, because each goal pulls in precisely
  the templates it needs.

  Goal-direction alone is not enough. `check_generic_bounds`
  (`items.rs:735`) computes both the *proof* and the *bound context*
  (`bound_context_for`) in one pass, and the bound context calls
  `conformances_for_type` — a full sweep — **in the middle of a proof**.
  Left in place, a fourth unrelated blanket swept in mid-proof reproduces the
  same false cycle through a different door. Nothing reads the bound context
  until body-check time (`bodies.rs:222`, `bodies.rs:307`, `compile.rs:698`),
  so computing it there instead is a phase correction, not a workaround:
  **a bound context is body-checking information; computing it during
  signature resolution is what made the query re-entrant.** It also deletes a
  field — `ConformanceEntry::bounds` collapses into `declared_bounds` — and
  removes an order-dependence of its own, since a context computed later sees
  strictly more registered conformances.

  Alternatives rejected:
  - *Widen the cycle guard's key.* It is already `(target, spec)`, which is
    the right key. The problem is what the query does around it.
  - *Fixpoint materialization per target.* A template's bound is frequently on
    a *different* type (`conform<T: Show> Wrapper<T> to Display` checks
    `T: Show`), so a per-target fixpoint does not compose. Goal-direction does.
  - *Resolve every template's spec at park time* so `solve` can filter without
    resolving. A generic template's spec arguments may reference its own
    generics (`conform<K, V> HashMap<K, V> to ToIterator<KeyValue<K, V>>`), so
    at park time there is nothing to bind them to. Resolving the spec once the
    target has *matched* — when the substitution is known — is exact and needs
    no new resolution entry point.

  **Part 2**: `docs/14` frames alias non-expansion as an `AmbiguousConformance`
  nuisance. The practical failure is worse and bidirectional (verified):

  ```
  spec AB = A + B;
  conform<T: AB> T to X { … }      use_x<T: A + B>(p: *T) => i32 { p.x() }
  conform<T: A + B> T to X { … }   use_x<T: AB>(p: *T) => i32 { p.x() }
  ```

  Both report `method 'x' comes from spec 'X' but is not in this bound
  context`. An alias that is not interchangeable with its members is not an
  alias. Both sites (`compare_conformance_precedence` and `bound_context_for`)
  key bounds as `(spec.id, args)` without expanding members; one shared
  normalizer fixes both.

  **Part 3**: the two return-type problems that look related are **duals**, not
  one problem. `f() => spec Animal` is existential (the callee picks the type;
  read it out of the body); `lowest<T: Bounded>() => T` is universal (the
  caller picks; read it from `expected`). Inference flows in opposite
  directions and they live in different places. Part 3 implements the
  universal half; part 4 deletes the existential half. That is the coherent
  outcome, not a compromise: after this, a return type is either written
  concretely, or chosen by the caller through an ordinary generic parameter.

  For the universal half the precedent already exists:
  `Analyzer::infer_literal_type_args` (`literals.rs:565`) consults `expected`
  *first*, before probing fields. So the call path should do the same — unify
  the declared return type against `expected` to **seed** the substitution,
  then let `infer_generic_args` run unchanged. Seeded generics then flow into
  `expected_for_generic_param`, so `y : i64 = identity(5)` adapts the literal
  instead of pinning `T = i32`. No new inference concept, and the same
  precedence order literals already use.

  `docs/14` says this is "a change to `resolve_generic_static_call` alone".
  **That is wrong**, and an executing agent following it would edit the wrong
  function: the documented repro is a *free* function, handled by
  `resolve_generic_call` → `finish_generic_call`. The two paths are
  distinguishable by their diagnostics — the free-function case emits
  `UnresolvedGenericParam` ("cannot infer type parameter 'T' from this call's
  arguments"), the generic-static case emits `UnresolvedLiteralGeneric`
  ("cannot infer type argument(s) 'T' of 'Box' here"). Both are real gaps and
  both are in scope; leaving one would be exactly the "two mechanisms for one
  concept" tax.

  **Part 4**: `spec T` in return position reads as "I will return some
  unknown type that implements XYZ". At a **definition site** that reading is
  false — the function always returns one specific type, and the author of
  that function knows exactly which one. The syntax promises a dynamism the
  semantics do not deliver, and the only thing it buys is hiding a name the
  author could simply have written.

  Inside a **spec declaration** the same syntax is honest, because there the
  statement is about implementors rather than about one body: "the implementor
  will return a known type which implements XYZ." Each implementor writes its
  own concrete return type, checked against the bound
  (`FlattenedSpecFn::return_type_bound`). Nothing is inferred. That position
  stays, and it is load-bearing — `core::iterator`'s
  `to_iterator(*self) => spec Iterator<T>` is what `for..in` runs on.

  The **parameter** position also stays, unchanged: it is pure sugar,
  desugared during HIR lowering into a bound generic parameter, and follows
  ordinary monomorphization exactly as if the author had written the generic
  out longhand.

  So the removal is narrow and the machinery behind it is not: deleting the
  definition-site case removes `Driver::resolve_spec_return_function`'s phase
  inversion (a throwaway probe body-check running *before* the signature can be
  resolved at all), `ItemQueries::spec_return_inference_stack`, the analyzer's
  `inferring_return_type`/`inferred_return_candidates` state, the
  `return_type_override` parameter threaded through
  `collect_function_signature`, and three error kinds — 54 references across
  7 files. **Verified: nothing in `runtime/` or `examples/` uses it.** The only
  `=> spec X` in the tree is `core::iterator`'s spec declaration, plus one
  comment in `examples/dev/dev.omg`.

  This also resolves the asymmetry that started this thread — a free function
  inferring what a method could not — by deleting the special case rather than
  extending it. The rule becomes uniform: **`spec T` in return position always
  means "declared bound, concrete type written by the implementor", never
  "inferred from the body".**

  **Part 5**: `Analyzer::require_mutable_place` (`places.rs:607`) picks
  `NotMutablePointer` for any root that is not an unqualified path, and its
  own doc comment states the assumption that justified that — "a non-place
  root … is never itself the *cause* of immutability". That assumption is no
  longer true: spec-qualified calls wrap a non-place receiver in
  `HirPlaceRoot::Expr`, producing exactly the shape the assumption excluded.
  `Bumpable::bump(make())` is correctly *rejected*, but the message names a
  pointer that does not appear in the source. A third error kind for the
  not-a-place case fixes it, and the stale doc comment must be corrected with it.

  For the slice case, the question — should `*T` reach slices at all — is
  settled: **no**. Omega already answered it; `[]T` is not a type
  (`error: '[]T' is not valid on its own`), only `*[]T` is, and it is a fat
  pointer (`ResolvedType::Slice`). So `f<T>(x: *T)` against a `*[]u8` would
  need `T = []u8`, which does not exist, and the *by-value* form already works
  today (verified: `use_it<T: Show>(s: T)` binds `T = *[]u8` and compiles).
  Rust reaches slices through `&T` only by opting out of `Sized` with
  `?Sized`, which makes a pointer's representation depend on a type parameter
  — the hidden cost Omega exists to refuse, and unnecessary here because
  `spec *T` already spells fat-pointer dispatch explicitly. This is therefore
  a **diagnostic bug, not an inference gap**: the message must teach the rule
  instead of blaming inference.

- **Resolved concerns:**
  - *Are the two return-type problems the same machinery?* No — duals. The
    universal half is implemented (part 3), the existential half removed
    (part 4).
  - *Should definition-site `spec T` returns be implemented for methods
    instead?* No. The syntax misdescribes what it does at a definition site,
    and supporting it needs body analysis during the signature phase in every
    compilation module — a depth of analysis not wanted here. Tracked in the
    docs with the reasoning, not silently dropped.
  - *Variadic spec functions.* Declared **unplanned**, not a gap. `...` is for
    C varargs interop in `extern` and for macros; nothing else in the language
    is planned to support it. Not banned forever — just not scheduled. The
    guard and its plumbing stay; the entry moves out of the issue tracker.
  - *Should `*T` reach slices?* No. Diagnostic-only, no `?Sized`, `*T` stays
    thin.

## Technical Details

### What changes

**`compiler/omega-driver/src/conformances.rs`** (part 1, 2)
- `InProgressConformance` → `ConformanceGoal { target, spec: HirId, id, span }`;
  `Conformances::in_progress` → `Conformances::goals`.
- New `Driver::solve(target, spec: Option<&Rc<RefCell<ResolvedSpecType>>>)`:
  the single place a template is ever instantiated for a target. `Some(spec)`
  restricts to templates producing that spec (the demand path); `None` sweeps
  every matching template (the "all conformances of this type" path). A
  template whose goal is already on the stack is skipped **silently** — only
  `conformance_for` reports.
- New private `Driver::template_spec(template, substitution)`: resolves a
  matched template's spec reference with its generics already bound, inside
  `Analyzer::probe` so a failure reports nothing here.
- `conformance_for`: look up, `solve(target, Some(spec))`, look up again,
  and only then report `ConformanceCycle` if the goal is on the stack.
- `materialize` → `solve(target, None)` plus the `materialized` memo, recorded
  only when the sweep skipped nothing (a partial sweep is not a complete one).
- `instantiate_conformance`: drop its own `(id, target)` in-progress guard
  (subsumed by the goal stack), stop calling `bound_context_for`, and record
  `Conformances::failed` **only when `goals.len() == 1`** — at the outermost
  goal nothing else in flight could have caused the failure, so it is genuine
  and permanent. (This is what keeps the already-fixed duplicate-
  `SpecNotImplemented` behaviour intact while making a nested failure retryable.)
- `ConformanceEntry`: delete `bounds`; keep `declared_bounds`; add
  `declared_bound_keys: Vec<(HirId, Vec<ResolvedType>)>` — the alias-expanded
  identity of `declared_bounds`, computed once where an analyzer is already in
  hand.
- `compare_conformance_precedence` compares `declared_bound_keys`.
- `bound_context_for` takes expanded keys for the item's declared set.
- Delete the local `alias_member_ids` in favour of the analyzer's.

**`compiler/omega-driver/src/items.rs`** (part 1, 4)
- `check_generic_bounds` returns only the *declared* bounds; the
  `bound_context_for` loop moves out.
- `ItemQueries::generic_bounds` becomes the **declared** bounds
  (`HashMap<ItemKey, Vec<ResolvedBound>>`, same shape, renamed to
  `declared_bounds` with a doc comment saying so).
- Delete `resolve_spec_return_function`, its dispatch branch in `compute_item`
  (`items.rs:933`), and `ItemQueries::spec_return_inference_stack`.

**`compiler/omega-driver/src/bodies.rs`, `compile.rs`** (part 1)
- The three sites that consume a bound context compute it there, via
  `Driver::bound_context_for`, from the stored declared set.

**`compiler/omega-driver/src/error.rs`** (part 4)
- Delete `ResolveError::SpecReturnTypeRecursion` and its rendering.

**`compiler/omega-analyzer/src/analysis/specs.rs`** (part 1, 2)
- New `Analyzer::probe<R>(f)`: runs `f` and discards every diagnostic it
  produced — names the speculative-question idiom the codebase already
  open-codes in `classify_for_in_source` and `probe_literal_type_args`. Put it
  next to the analyzer's other cross-cutting helpers (`analysis/mod.rs`), not
  in `specs.rs`.
- New `Analyzer::expand_bound_set(id, span, bounds) -> Vec<(HirId, Vec<ResolvedType>)>`:
  flattens a bound set through every alias it names, resolving each member's
  raw type arguments under the alias's own generics — mirroring exactly what
  `flatten_spec_into` (`specs.rs:802`) already does for the same data, for the
  same reason.
- `alias_member_ids` becomes the single shared implementation (public, in the
  analyzer; the driver's `Vec`-based copy is deleted).
- `type_implements_spec`'s alias fallback (`specs.rs:927`) stops calling
  `conformances_for_type` and asks only for the alias's member specs, so it
  cannot trigger a full sweep from inside a proof.

**`compiler/omega-analyzer/src/resolver.rs` + `compiler/omega-driver/src/resolver.rs`** (part 3)
- `GenericSignature` and `GenericStaticFunctionSignature` gain
  `return_type: Type` (raw, unresolved).
- `generic_function_signature` fills it from `f.return_type`.
- `generic_static_function_signature` fills it from `f.return_type` with
  `Self` rewritten to the owner's own generic spelling
  (`Type::Generic(owner_name, owner_generics)`), so a `=> Self` static unifies
  against an expected `Box<i32>` the same way an explicitly written
  `=> Box<T>` does.
- New `ModuleResolver::conformances_for_specs(target, spec_ids)` for
  `type_implements_spec`'s alias fallback.

**`compiler/omega-analyzer/src/analysis/mod.rs`** (part 3, 4)
- `infer_generic_args` takes a starting substitution instead of beginning
  empty. Everything else about it is unchanged — the seed simply flows into
  `expected_for_generic_param` like any earlier argument's own pin.
- Delete the `inferring_return_type` and `inferred_return_candidates` fields.

**`compiler/omega-analyzer/src/analysis/items.rs`** (part 4)
- Delete `infer_body_return_type` and `collect_function_signature`'s
  `return_type_override` parameter (every caller then passes nothing).

**`compiler/omega-analyzer/src/analysis/stmts.rs`** (part 4)
- Delete the `HirStmt::Return` arm's candidate recording.

**`compiler/omega-analyzer/src/context.rs`** (part 4)
- Reword `Type::SpecStatic`'s arm comment: the two legitimate positions are
  now a parameter type and a spec's own function declaration.

**`compiler/omega-analyzer/src/analysis/calls.rs`** (part 3, 5)
- `resolve_generic_call` / `resolve_generic_static_call`: `_expected` becomes
  `expected` and is threaded to their `finish_*` halves.
- `finish_generic_call` / `finish_generic_static_call`: build the seed by
  unifying the declared return type against `expected` (widening each seeded
  type with `ResolvedType::widened`, the same rule `resolve_inferred_type_args`
  applies, so a caller's enum-variant refinement can never mint a spurious
  instantiation), then proceed unchanged.
- When inference still fails and the cause is a `*T` parameter against a fat
  pointer, report the dedicated diagnostic instead of `UnresolvedGenericParam`.

**`compiler/omega-analyzer/src/analysis/places.rs`** (part 5)
- `require_mutable_place` gains the not-a-place arm, and its doc comment's
  now-false assumption is corrected.

**`compiler/omega-analyzer/src/error/kind.rs` + `error/render.rs` + `error/mod.rs`**
- New: `MutateTemporary`, `GenericParamFromFatPointer`.
- Delete: `AmbiguousSpecReturnType`, `SpecReturnTypeUnconstrained`.
- Reword `SpecStaticNotAllowedHere`'s message and label so they name the two
  surviving positions instead of advertising the removed one.
- `ConformanceCycle` gains the goal chain so it prints what actually depends
  on what.

**Docs**: `08-specs.md`, `06-generics.md`, `11-strings-casting-and-slices.md`,
`14-known-issues.md`, and `docs/plan/` archival.

### What must not change

- **`spec T` in a spec's own function declaration.** `to_iterator(*self) =>
  spec Iterator<T>;` stays exactly as it is, along with
  `FlattenedSpecFn::return_type_bound`, the bound check against each
  implementor's concrete return type, and `ResolvedSpecType::is_object_safe`
  (which is computed from this position and nothing else). `core::iterator`
  and every `for..in` loop depend on it.
- **`spec T` in parameter position.** Pure sugar, desugared at HIR lowering
  into a bound generic parameter. Untouched.
- **`spec *T`** (`Type::SpecObject`) in any position, including return
  position — `=> spec *AB` is a genuine fat-pointer return and is unrelated to
  the removal.
- **No `?Sized`, no unsized type parameters, no representation-varying
  pointers.** `*T` stays a thin pointer. `[]T` stays not-a-type.
- **No variadic spec functions.** `VariadicSpecFunctionUnsatisfiable` and its
  `is_variadic` plumbing stay exactly as they are. This is a docs
  reclassification only.
- **Demand-driven conformance emission** (a blanket emitting a body per
  materialized type) is out of scope. Goal-direction shrinks it as a side
  effect — the demand path stops instantiating every template on a type — but
  the real fix is a change to how `check_conformance_bodies` is scheduled. It
  costs object size and compile time only; `--gc-sections` already keeps the
  dead copies out of the executable. Keep its known-issues entry, and update
  it to note the reduction rather than claiming a fix.
- **Lazy blanket body checking** and **latent blanket overlap diagnosed at
  use** stay as they are: both are deliberate and correctly documented.
- **Generic *methods*** (as opposed to generic free functions and generic-type
  statics) remain outside call-site inference — `resolve_generic_call`
  declines method-shaped calls by design, and this plan does not change that.
- **Overload resolution ordering.** `resolve_overloaded_call` still runs before
  `resolve_generic_call`; a generic catch-all still cannot coexist with
  concrete overloads. Untouched.
- The **spec-qualified call ladder** (`S::f()` / `P::f()` / `<S : P>::f()`)
  and its `expected` threading are already correct. Part 3 consumes that
  threading; it must not alter it.

### Chosen approach

The unifying idea across parts 1 and 4 is the same: **do work in the phase
that owns it.** Bound contexts belong to body checking, not signature
resolution. A return type belongs to the signature phase, which is why a
construct that could only be resolved by inverting that order is being removed
rather than extended. Both changes make the pipeline shorter.

Part 3's approach is chosen for consistency rather than novelty: literals
already prefer `expected` over structural probing, so calls should too, using
the identical substitution-seeding mechanism.

Part 5 is chosen to be honest rather than clever: two diagnostics that
currently describe a construct the author never wrote are replaced with ones
that describe the rule they actually broke.

### Risks and open questions

- **A template whose spec name does not resolve** is now skipped silently by
  the demand path (`solve(target, Some(spec))` cannot match a spec it cannot
  resolve). The full-sweep path still instantiates it and still reports, so the
  diagnostic survives for any type that ever gets a method lookup or bound
  context. Add a regression test; if it turns out a realistic program can lose
  the diagnostic entirely, flag it rather than silently accepting.
- **Seeding from `expected` could pin a generic the arguments would have
  bound differently.** By design the seed wins (it is the outermost
  constraint, matching literals), and the unchanged final `accepts` loop still
  rejects any genuine mismatch — so the failure mode is a differently-worded
  error, never a wrong acceptance. Watch for existing tests whose *wording*
  changes, and if any existing program stops compiling, that is a real finding
  to report, not a test to update.
- **`ConformanceEntry::bounds` deletion** touches codegen's conform-body
  seeding (`compile.rs:698`). Verify the seeded bound list is identical
  before/after by comparing `nm --defined-only` output on `target/core.o` and
  `target/std.o`.
- **Part 4 is a removal, so its risk is what it takes with it.** Confirm by
  grep that `Type::SpecStatic` still reaches exactly two consumers after the
  deletion — HIR lowering's parameter desugaring, and
  `resolve_raw_spec_fn_type`/`is_object_safe` for the spec-declaration
  position. If a third consumer survives, stop and report it rather than
  deleting it.

## Implementation Plan

Each step must leave `cargo build`, `cargo test`, and `just build-core` working.

### Part A — goal-directed conformance solving

1. **Add `Analyzer::probe`.** In `compiler/omega-analyzer/src/analysis/mod.rs`,
   add `pub fn probe<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R` that
   snapshots `self.errors.len()`/`self.warnings.len()`, runs `f`, and truncates
   both back. Document it as "a speculative question whose failure is not this
   query's to report; the real path re-derives and reports it." Do **not**
   retrofit `classify_for_in_source` or `probe_literal_type_args` — they keep
   diagnostics on outright failure, which is a different contract.

2. **Move bound-context computation to body-check time.**
   - `Driver::check_generic_bounds` (`items.rs:735`): delete the trailing
     `bound_context_for` loop; return `Result<Vec<ResolvedBound>, ResolveError>`
     carrying only `declared`.
   - `check_item_generic_bounds` stores that into `ItemQueries::generic_bounds`,
     renamed `declared_bounds` with a doc comment stating it is the *declared*
     set, not the context.
   - `bodies.rs:222` and `bodies.rs:307`: call `Driver::bound_context_for` over
     the stored declared set to build the `bounds` passed to `with_analyzer_in`.
   - `instantiate_conformance` stores only `declared_bounds` on the entry;
     delete `ConformanceEntry::bounds`.
   - `compile.rs:698`: build the conform body's bound list from
     `entry.declared_bounds` via `bound_context_for` (which already seeds
     `(target, spec, args)` itself — drop the manual seed if it becomes
     duplicated).
   - Verify with `nm --defined-only` on `target/core.o` and `target/std.o`
     that the symbol set is byte-identical to before this step.

3. **Introduce the goal stack.** In `conformances.rs`, replace
   `InProgressConformance`/`Conformances::in_progress` with
   `ConformanceGoal { target: ResolvedType, spec: HirId, id: HirId, span: Span }`
   and `Conformances::goals: Vec<ConformanceGoal>`. Keep behaviour identical
   for now (push/pop at the same points); this step is a pure rename so the
   next one is reviewable.

4. **Add `Driver::template_spec`.** Private to `conformances.rs`: given a
   template and the substitution `match_conform_target` produced, resolve
   `template.conform.spec` through `with_analyzer` + `Analyzer::probe` +
   `resolve_spec_reference`, returning `Option<(cell, args)>`. Document why the
   substitution is required (`conform<K, V> HashMap<K, V> to
   ToIterator<KeyValue<K, V>>`) and why failures are silent here.

5. **Add `Driver::solve` and rewrite the two queries.**
   - `fn solve(&mut self, target: &ResolvedType, spec: Option<&Rc<RefCell<ResolvedSpecType>>>) -> SweepOutcome`
     where `SweepOutcome` records whether any template was skipped because its
     goal was already on the stack. For each template: `match_conform_target`;
     if `spec` is `Some`, `template_spec` and skip on id mismatch; skip
     silently if `(target, spec_id)` is already a goal; otherwise push the
     goal, `instantiate_conformance`, pop.
   - `conformance_for`: registered-lookup → `solve(target, Some(spec))` →
     registered-lookup → report `ConformanceCycle` only if the goal is on the
     stack. It must no longer call `materialize`.
   - `conformances_for_type`: `solve(target, None)`, recording the
     `materialized` memo only when nothing was skipped.
   - `instantiate_conformance`: delete its own in-progress guard and its
     `bound_context_for` call (already gone from step 2); gate the
     `Conformances::failed` push on `self.conformances.goals.len() == 1`.
   - Delete `materialize`.

6. **Make `type_implements_spec`'s alias fallback goal-directed.** Add
   `ModuleResolver::conformances_for_specs(target, spec_ids) -> Vec<ResolvedConformance>`
   (driver side: `solve(target, Some(spec))` per id, then filter entries) and
   use it at `specs.rs:927` in place of `conformances_for_type`. This is what
   guarantees no full sweep can run inside a proof.

7. **Make `ConformanceCycle` print the chain.** Extend the error kind with the
   ordered goal chain (target string, spec name, span per link) and render one
   `note:` per link, e.g.

   ```
   error: cyclic conformance while proving 'S: A'
     = note: proving 'S: A' requires 'S: B'
     = note: proving 'S: B' requires 'S: A'
   ```

8. **Invert the pinned test.** Replace
   `a_blanket_chain_is_currently_misreported_as_a_cycle` in
   `compiler/omega-driver/tests/conform.rs:1894` with a test asserting the
   chain compiles, and add its mirror with the two blankets declared in the
   opposite order — order-independence is the actual property being fixed.
   `a_genuine_conformance_cycle_is_rejected` (line 1863) must still pass.

### Part B — spec-alias transparency

9. **One `alias_member_ids`.** Make the analyzer's (`specs.rs:876`) public and
   delete the driver's copy (`conformances.rs:963`), adapting its `Vec` callers.

10. **Add `Analyzer::expand_bound_set`.** In `specs.rs`, mirroring
    `flatten_spec_into`'s alias-argument resolution (`specs.rs:802`): for each
    bound, emit `(spec.id, args)` plus, transitively, every alias member's
    `(id, resolved args)`, resolving each member's raw arguments under the
    alias's own generics bound to the bound's arguments. Resolve against the
    alias's own module, exactly as `flatten_spec_into` does.

11. **Use it in both places.**
    - `instantiate_conformance`: compute `declared_bound_keys` from
      `declared_bounds` right after the bound check, store it on the entry, and
      have `compare_conformance_precedence` compare those instead of re-keying.
    - `bound_context_for`: expand the item's `declared` set before the
      entailment test, and expand each candidate entry's own bounds by reading
      its stored `declared_bound_keys`.

12. **Invert the pinned test.** Replace
    `an_alias_bound_and_its_inline_spelling_do_not_compare_as_equal`
    (`conform.rs:1924`) with one asserting `DuplicateConformance` (equal bound
    sets), and add the two bound-context cases from the Reasoning section —
    alias-declared blanket used under an inline bound, and the reverse — as
    compiling programs.

### Part C — return-type-driven generic inference

13. **Carry the return type.** Add `return_type: Type` to `GenericSignature`
    and `GenericStaticFunctionSignature` (`omega-analyzer/src/resolver.rs:630,655`).
    Fill it in `generic_function_signature` (`omega-driver/src/resolver.rs:300`)
    and `generic_static_function_signature` (`:381`). For the static case,
    rewrite a `Self` leaf to `Type::Generic(owner name path, owner generics as
    Named types)` with a small recursive helper covering the same shapes
    `unify_generic_type` walks; document why (`=> Self` must unify against an
    expected `Box<i32>` exactly as a written `=> Box<T>` does).

14. **Seed the substitution.** Change `Analyzer::infer_generic_args`
    (`analysis/mod.rs:971`) to take a starting `HashMap<Ident, ResolvedType>`
    instead of creating an empty one. Update its three callers
    (`finish_generic_call`, `finish_generic_static_call`,
    `probe_literal_type_args`) — the last one passes an empty map.

15. **Thread `expected` and build the seed.** In `calls.rs`, rename `_expected`
    to `expected` in `resolve_generic_call` (`:2323`) and
    `resolve_generic_static_call` (`:1735`) and pass it on. In
    `finish_generic_call` (`:2373`) and `finish_generic_static_call` (`:1832`),
    when `expected` is `Some`, `unify_generic_type` the signature's declared
    return type against it into a fresh map, apply `ResolvedType::widened` to
    every seeded entry, and hand that to `infer_generic_args`. Document the
    precedence in one place: **expected type > argument-driven inference >
    declared default**, and that it matches `infer_literal_type_args`'s
    existing order.

### Part D — remove definition-site `spec T` return types

16. **Delete the driver's inversion.** In `compiler/omega-driver/src/items.rs`:
    remove the `if let Type::SpecStatic(bound) = &f.return_type` branch in
    `compute_item` (`:933`) so a function definition always takes the ordinary
    `collect_function_signature` path; delete `resolve_spec_return_function`
    (`:1178`) and `ItemQueries::spec_return_inference_stack` (`:391`); delete
    `ResolveError::SpecReturnTypeRecursion` and its rendering from
    `omega-driver/src/error.rs`.

17. **Delete the analyzer's inference.** In `omega-analyzer`: delete
    `Analyzer::infer_body_return_type` (`analysis/items.rs:1287`); delete the
    `return_type_override` parameter from `collect_function_signature` and drop
    the argument at every call site; delete the `inferring_return_type` and
    `inferred_return_candidates` fields (`analysis/mod.rs`) and the
    `HirStmt::Return` arm in `analysis/stmts.rs` that populates them; delete
    `AmbiguousSpecReturnType` and `SpecReturnTypeUnconstrained` from
    `error/kind.rs` and `error/render.rs`.

18. **Make the surviving diagnostic accurate.** With step 16 done, a
    definition-site `=> spec Bound` now falls through to `Context::resolve_type`
    and produces `SpecStaticNotAllowedHere` on its own — no new error kind is
    needed. Reword it so it stops advertising the position that was just
    removed:
    - message (`error/mod.rs:208`): "'spec Animal' is only allowed as a
      parameter type, or as a return type inside a spec's own function
      declaration"
    - label (`error/render.rs:741`): "`spec ...` (static dispatch) is not a
      concrete type, and a function definition must name one"
    - help: "name the concrete type this returns, or take the caller's choice
      as a bound generic parameter (`f<T: Animal>() => T`)"
    - correct the `Type::SpecStatic` arm comment in `context.rs:407` to name
      the two surviving positions.
    Then confirm `a_spec_return_type_on_a_method_is_rejected_not_inferred`
    (`conform.rs:1659`) still passes, and add its free-function sibling.

### Part E — diagnostics

19. **`cannot mutate a temporary value`.** Add
    `AnalysisErrorKind::MutateTemporary`. In `require_mutable_place`
    (`places.rs:607`), select it before the `through_pointer` test when the
    checked place's root is `CheckedPlaceRoot::Expr` and there is no `Deref`
    projection. Message: "cannot mutate a temporary value"; note: "`*mut self`
    writes through a pointer to the receiver, and this receiver is a
    freshly-produced value, not a place"; help: "bind it to a `mut` local
    first". Correct the doc comment's now-false assumption in the same edit.

20. **Teach the slice inference failure.** Add
    `AnalysisErrorKind::GenericParamFromFatPointer { parameter: Ident, found: ResolvedType }`.
    In `finish_generic_call` and `finish_generic_static_call`, before emitting
    `UnresolvedGenericParam`/`UnresolvedLiteralGeneric`, scan the raw parameter
    types against the checked argument types for a `Type::Pointer(Type::Named(g))`
    (where `g` is the unbound generic) matched against a `ResolvedType::Slice`
    or `ResolvedType::Str`, and report this instead. Message: "cannot infer
    type parameter 'T' from this call's arguments"; note: "'*[]u8' is a slice
    — a pointer with a length — so it does not match the thin pointer '*T'";
    help: "take the value directly (`x: T`), or spell the slice out
    (`x: *[]T`)".

### Part F — documentation

21. **`docs/14-known-issues.md`**: remove the entries fixed here. Remove the
    two stale ones — the duplicate `SpecNotImplemented` (already fixed by
    `Conformances::failed`) and "only observable by execution" (that is
    `examples/spec_dispatch` existing, not a gap). Move the variadic-spec entry
    out entirely. Replace the "`spec T` return type on a method is rejected"
    entry with a **design note**: definition-site `spec T` returns are removed,
    the reopening condition is a compiler that can afford body analysis during
    the signature phase, and the workaround is to name the concrete type or
    take a bound generic parameter. Update the blanket-emission entry to record
    the reduction goal-direction brings without claiming a fix.

22. **`docs/08-specs.md`**: state variadic-in-specs as a design decision, not a
    limitation. Delete the "Return position, on an ordinary (non-spec)
    function" subsection and replace it with the rule and its rationale:
    `spec T` in return position promises "some unknown type implementing XYZ",
    which is true of a *spec declaration* (each implementor answers
    differently) and false at a definition site (one body, one type, known to
    its author) — so it is allowed in the former and rejected in the latter,
    and the parameter position is unaffected sugar. Document goal-directed
    conformance solving and that alias bounds and their inline spelling are
    interchangeable everywhere.

23. **`docs/06-generics.md`**: document the call-site precedence — expected
    type > arguments > default — and that it covers both free generic
    functions and generic-type statics. Note that this is now the *only* way a
    return type is chosen by anything other than the definition.

24. **`docs/11-strings-casting-and-slices.md`**: state the rule the new
    diagnostic teaches — `[]T` is not a type, `*[]T` is a fat pointer, `*T` is
    always thin, and a slice binds a bare type parameter. Record the accepted
    consequence: a generic that must accept both aggregates and slices takes
    `x: T`, which copies an aggregate.

25. **Archive** this plan as `docs/plan/0014-spec-and-conformance-resolution.md`
    and delete `PLAN.md`.

## Testing

### New cases

- **Part A** (`compiler/omega-driver/tests/conform.rs`): a three-link blanket
  chain compiles in *both* declaration orders; a chain whose middle link is a
  concrete conform still works; a fourth blanket bounded on the chain's middle
  spec compiles (this is the case that fails if step 2 is skipped); a generic
  template whose spec name does not resolve still reports `NotASpec`.
- **Part B**: alias-declared blanket reached under an inline bound and the
  reverse both compile; alias and inline blankets for the same spec report
  `DuplicateConformance`; a generic alias (`spec Both<T> = Iter<T> + Eq`)
  expands with its arguments substituted.
- **Part C**: `lowest<T: Bounded>() => T` callable at every `expected`
  position already verified for the spec ladder — declaration annotation, tail
  return, explicit `return`, argument, `if` branch, array element;
  `a : Box<i32> = Box::empty()` with `=> Self` and with `=> Box<T>`;
  `y : i64 = identity(5)` adapts the untyped literal; an argument's own
  explicit type still wins over `expected`.
- **Part D**: `to_iterator(*self) => spec Iterator<T>` inside a spec
  declaration still compiles and still drives `for..in` (covered by
  `just test-range` and `just run-exec`); a `spec T` parameter still desugars
  and monomorphizes; `=> spec *AB` still compiles (`conform.rs:377`, `:1350`).
- **Part E**: `Bumpable::bump(make())` reports `MutateTemporary`;
  `f<T: Show>(x: *T)` called with a `*[]u8` and with a `*str` reports
  `GenericParamFromFatPointer`; the by-value form still compiles.
- **Runtime**: extend `examples/spec_dispatch/` with a blanket chain whose
  body actually runs, returning a distinct exit code per failed case, so
  "which body ran" stays an executed fact rather than a declaration-level one.

### Negative cases

- A genuine two-blanket cycle still reports `ConformanceCycle`, and now prints
  the chain that closes it.
- `f() => spec Animal { Dog{} }` on a free function, and the same on a method,
  both report `SpecStaticNotAllowedHere` with the reworded message — which
  must not mention a definition-site return type as legal.
- `f(x: []u8)` still reports `'[]T' is not valid on its own` — the slice rule
  is unchanged; only the *inference* diagnostic is new.
- `conform T to SomeAlias` still reports `ConformToAliasSpec`. Alias expansion
  affects *bounds*, never conformance targets.
- A variadic spec function still reports `VariadicSpecFunctionUnsatisfiable`
  (`conform.rs:1639` must pass untouched).
- A spec whose declaration has a `spec T` return is still not object-safe:
  `spec *ToIterator<T>` still reports `SpecNotObjectSafe`. Part 4 must not
  touch `is_object_safe`.

### Regression risk

- Highest: **part A step 5**. Every conformance answer flows through it. The
  bellwethers are `just test-spec-dispatch`, `just test-spec-calls`, and
  `just test-multi-print` (which is what caught the last conformance
  regression, since it links two packages that each instantiate a template).
- **Part A step 2** can silently change which bounds a conform body sees.
  Guard it with the `nm --defined-only` diff on `target/core.o` (226 symbols)
  and `target/std.o` (91 symbols) — both counts must be unchanged by the whole
  plan, since nothing here adds or removes a conformance.
- **Part C** can change diagnostic *wording* on existing failing-inference
  tests. Wording changes are fine; a program that stops compiling is a real
  finding to report, not a test to update.
- **Part D** removes shared analyzer state, so the compile errors it produces
  are the map of what depended on it. If anything outside the listed files
  breaks, stop and report — it means a consumer this plan did not account for.
  `just test-range` is the direct guard on the spec-declaration position
  surviving intact.
- Full gate list, all of which must pass: `cargo test`, then `just test-io
  test-stdio-contract test-core-only test-root-layout test-allocator-only
  test-multi-print test-range test-char test-spec-dispatch test-spec-calls
  run-exec`. The build must stay warning-clean.

### Target coverage

`core`-only (`just test-core-only`, `test-range`, `test-char`) is the
freestanding, no-allocator assertion and covers every part of this plan —
none of it introduces allocation, runtime support, or a hosted dependency.
`just run-exec` covers the hosted path end to end.
