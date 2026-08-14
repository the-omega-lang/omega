# Blanket conformance

## Task Description

- **What is being asked:** allow a `conform` block whose target is one of its
  own generic parameters, so a single declaration conforms every type that
  already satisfies some bound:

  ```
  exposed spec Sum { sum(*self, other: *Self) => Self; }

  conform<T: Numeric> T to Sum {
      sum(*self, other: *Self) => Self { self.add(other) }
  }
  ```

  The orphan rule and every other conformance rule still apply. One new rule:
  **a more specific conformance always beats a less specific one.** Given
  `conform<T: MyFirstSpec> T to MySecondSpec` and an explicit
  `conform String to MySecondSpec`, `String` uses the explicit block.

- **Purpose:** today the only way to give N types a spec is N `conform`
  blocks. `std::primitives` already pays this: `macro signed_integer($T)`
  exists purely to stamp out `Ord`/`Default`/`Hash`/`Display` per scalar,
  which is a macro standing in for a missing language feature. Blanket
  conformance replaces that with one declaration per spec, and lets a package
  express "anything countable is printable" without enumerating types.

- **Reasoning:** the mechanism is already 80% present. `conform<T> List<T> to
  ToIterator<T>` is a *template* matched structurally against concrete targets
  and instantiated lazily (`Conformances::templates`,
  `Driver::match_conform_target`, `Driver::instantiate_conformance`). A blanket
  is the same thing with a target that matches everything and a bound that does
  the filtering. What is genuinely new is **precedence**: today at most one
  declaration can produce an entry for a given `(target, spec, spec args)`, so
  no selection is needed. Blankets break that, and the selection rule has to be
  designed rather than discovered.

  Rejected alternatives:
  - *Select at lookup, keep every matching entry in the registry.* Rejected:
    `conformances_for_type` feeds `bound_context_for` and the static-path
    resolver in `analysis/paths.rs:720`, which reports
    `AmbiguousConformanceStatic` when two entries provide the same static
    function. Leaving losers in the registry makes every specialized blanket
    look ambiguous, and `check_conformance_bodies` would emit both bodies.
  - *Upfront overlap checking between declarations (Rust's coherence).*
    Rejected as premature: it needs a "do these two bounds intersect" judgement
    over specs that may not overlap for any type in the program, and it reports
    errors for pairs no program ever triggers. Deciding overlap lazily, at the
    concrete target that makes two declarations collide, never yields a false
    positive. The cost is stated under *Risks*.
  - *A `specialize`/`default` keyword (Rust's `min_specialization`).* Rejected:
    the user's rule needs no opt-in marking, and adding one would make the
    common case (`conform String to Sum` beats the blanket) require ceremony.

- **Resolved concerns** — four decisions taken on the user's behalf, each a
  gap in the stated rules rather than a contradiction of them:

  1. **The orphan rule collapses to "the spec must be local" for a blanket.**
     `Driver::check_conformance_orphan` accepts a conform when *either* the
     target's package or the spec's package is the declaring one. A blanket's
     target ranges over every type, including foreign ones, so target-locality
     can never hold in general. Checking it per instantiation would report an
     orphan violation at whatever unrelated use site first instantiated the
     template. So a blanket is checked **at its declaration** and requires the
     spec to be package-local. This matches Rust, which likewise rejects
     `impl<T> ForeignTrait for T`.

  2. **Two matching declarations with no specificity relation are a hard
     error, not a silent pick.** The user's rule orders specific-vs-blanket. It
     does not order `conform<T: Ord> T to S` against `conform<T: Eq> T to S`
     for a type satisfying both. Proposal: one blanket is more specific than
     another when its bound's spec **transitively depends on** the other's —
     `spec Ord : Eq` makes `T: Ord` strictly more specific than `T: Eq`. That
     is decidable, nominal, and reuses `Driver::transitive_dependency_ids`,
     which already exists in `conformances.rs:667` for exactly this shape of
     question. Anything else is `AmbiguousConformance`.

  3. **Mutually recursive blankets terminate by cycle detection.**
     `conform<T: A> T to B` plus `conform<T: B> T to A` makes
     `instantiate_conformance` → `check_generic_bounds` →
     `Analyzer::type_implements_spec` → `conformance_for` →
     `instantiate_conformance` loop forever. Proposal: an in-progress stack
     keyed on `(declaration id, target)`; re-entry answers "not satisfied" and
     reports `ConformanceCycle` once. Note the recursion cannot diverge any
     other way: a bound is always checked against the *same* type the
     conformance is being sought for, so no target ever grows.

  4. **`derived` and `from_template` become one ordered precedence key.** The
     "which entry wins" question already exists in the code as two booleans
     with an implicit relationship (`instantiate_conformance:502` evicts
     derived entries, `conformance_for:639` prefers non-derived). Blanket adds
     a third axis. Rather than a third boolean, all three collapse into
     `(origin, role)` — see *Chosen approach*.

## Technical Details

### Architectural spaghetti this must clean up first

These are pre-existing and all sit directly under the feature. Fixing them is
step 1–3, before any behaviour changes:

- **"Is this a template instantiation?" is encoded three different ways.**
  `ConformanceEntry::from_template` (`conformances.rs:41`, drives codegen
  linkage), `entry.substitution.len() > 1` (`compile.rs:649` and
  `compile.rs:809` — drives who owns the body), and
  `entry.substitution.len() != 1` (`compile.rs:1003` — drives extern-function
  import, and note the sense is inverted there). Nothing keeps the three in
  step, and a blanket entry has the same shape as a generic one, so a
  divergence here becomes a wrong-linkage or duplicate-symbol bug exactly like
  the one `just test-multi-print` was added to catch. (`compile.rs:791` and
  `:985` are the same idiom for `PrimitiveEntry`; primitives have exactly one
  template shape and no precedence question, so they keep the length check —
  but give it a named helper rather than leaving a bare `len()` comparison.)

- **Precedence is two ad-hoc booleans.** `derived` and `from_template` are
  compared in two places with hand-written orderings
  (`instantiate_conformance:502`'s `retain`, `conformance_for:639`'s
  `.find(...).or_else(...)`). A third axis makes this unmaintainable.

- **`blanket_parameter` (`conformances.rs:735`) conflates two unrelated
  errors.** A bare-parameter target (`conform<T: Numeric> T to Sum`) and an
  unbindable parameter (`conform<T, U: Foo> List<T> to Bar`, where nothing can
  ever determine `U`) both report `BlanketConformanceNotYetSupported`. The
  first becomes legal; the second stays an error forever and needs its own
  diagnostic.

- **Target admissibility is split across two functions that must agree by
  hand.** `Analyzer::resolve_conform_target` (`analysis/specs.rs:112`) rejects
  pointer/array/function/spec-object targets; `template_target_is_matchable`
  (`conformances.rs:376`) separately whitelists the shapes
  `match_conform_target` can bind, with a comment explicitly saying the two
  "must agree or a legal target gets refused". A blanket must bind only to
  types that are admissible targets, so this becomes a third copy unless
  unified.

- **`conformance_for` and `conformances_for_type` each walk the template list
  with their own copy of the instantiate loop** (`conformances.rs:644` and
  `:713`). Blanket precedence has to be applied in both, so the duplication
  must go.

### What changes

| File | Change |
|---|---|
| `omega-driver/src/conformances.rs` | The bulk. New `ConformanceOrigin`/`ConformanceRole`, `materialize`, precedence-aware registration, blanket matching, cycle guard, two-pass collection. |
| `omega-driver/src/compile.rs` | Three `substitution.len()` checks replaced by `entry.origin`. |
| `omega-analyzer/src/analysis/specs.rs` | `check_conform_block` takes a pre-resolved spec; new `is_conformable_target`. |
| `omega-analyzer/src/error/kind.rs`, `error/render.rs` | Remove `BlanketConformanceNotYetSupported`; add four kinds. |
| `omega-analyzer/src/checked.rs`, `omega-codegen/src/cranelift/item.rs` | `ConformanceOwner::from_template` → `monomorphized`, derived from `origin`. |
| `docs/08-specs.md`, `docs/14-known-issues.md` | Document blankets, precedence, the tightened orphan rule; drop the "blanket out of scope" note. |
| `compiler/omega-driver/tests/conform.rs` | New cases; rewrite `blanket_conformances_are_rejected_with_their_own_diagnostic`. |
| `examples/dev/dev.omg`, `tests/*.expected` | A runtime case proving *which* body a specialized call runs. |

### What must not change

- **Spec-namespace resolution.** Blanket methods are reached exactly like every
  other conformance method — through a `T: Spec` bound or `Spec::method(x)`.
  They do not join the target type's inherent namespace. No change to
  `analysis/calls.rs` or `analysis/paths.rs` resolution rules.
- **Linkage and mangling.** A blanket instantiation is a monomorphization, so
  it takes the same `Linkage::Preemptible` path a generic instantiation already
  takes (`cranelift/item.rs:125`) and the same
  `mangle::conformance_method_symbol`. No new symbol shape; `core`/`std`/`plat`
  symbol tables must not move for programs that declare no blanket.
- **`primitive` blocks.** Untouched. `Primitives` keeps its own template list;
  no blanket primitives (a primitive is core-only and inherently per-type).
- **`std::primitives`' macros.** Migrating them onto blankets is a *follow-up*,
  deliberately out of scope: it would change which symbols `std.o` defines and
  entangle a language-feature change with a library rewrite.
- **Upfront overlap checking.** Explicitly not attempted (see *Reasoning*).

### Chosen approach

**One ordered precedence key replaces both booleans.**

```rust
/// How specific a conformance declaration is. `Ord` *is* the specialization
/// rule -- a more specific declaration always supersedes a less specific one
/// for the same `(target, spec, spec args)`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ConformanceOrigin {
    /// `conform<T: Bound> T to Spec` -- the target is a bare parameter.
    Blanket,
    /// `conform<T> List<T> to Spec` -- a type constructor with holes.
    Generic,
    /// `conform List<i32> to Spec` -- written out.
    Concrete,
}

/// Whether this entry owns a body or merely stands in for a spec dependency.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ConformanceRole { Derived, Declared }
```

`ConformanceEntry::precedence() -> (ConformanceOrigin, ConformanceRole)`,
compared lexicographically. This reproduces today's two rules exactly — a
declared entry beats a derived one at equal origin; a concrete conform beats a
blanket — and orders the new pair the right way round: a derived stand-in from
`conform Foo to Derived` beats a blanket `conform<T: X> T to Base`, because the
author said something specific about `Foo`.

`from_template` is then *derived*, not stored: `origin != Concrete`. That
single definition replaces all three of today's encodings.

**Blanket-vs-blanket is a partial order, resolved by bound subsumption.**
Two entries at `ConformanceOrigin::Blanket` are compared by their declaration's
bound: `A` is more specific than `B` when `B ∈ transitive_dependency_ids(A)`.
Unrelated bounds → incomparable → `AmbiguousConformance`.

**Registration decides; lookup only queries.** `instantiate_conformance` is
restructured so the cheap, diagnostic-free work happens before the expensive
work:

```
resolve target                       (already: resolve_conform_target)
resolve spec reference               (new: hoisted out of check_conform_block)
look up the incumbent for (target, spec, args)
    incumbent strictly more specific -> return quietly, no body check
    incumbent strictly less specific -> evict it, continue
    equal precedence                 -> DuplicateConformance
    incomparable                     -> AmbiguousConformance
check generic bounds                 (this is the blanket's applicability test)
check_conform_block                  (requirements <-> methods)
register + register_derived_conformances
```

Hoisting spec resolution is what makes "return quietly" possible: the registry
key is known before `check_conform_block` runs, so a losing blanket produces no
diagnostics from a body that will never be emitted. This also subsumes the
existing `retain`-based derived eviction at `conformances.rs:502` and
`reject_duplicate_conformance` into one function.

**One materialization primitive replaces two template walks.**

```rust
/// Instantiate every template that matches `target`, in declaration order.
/// Idempotent: the first call per target does the work, later calls are a
/// membership test. Sound because *all* templates are parked before any
/// concrete conform is registered (see the two-pass collection below).
fn materialize(&mut self, target: &ResolvedType);
```

`conformance_for` and `conformances_for_type` both become `materialize(target)`
followed by a plain query over `entries`. Because registration already resolved
precedence, at most one entry survives per `(target, spec, args)` and both
callers get the right answer with no selection logic of their own.

**Two-pass collection.** `collect_conformance_signatures` currently interleaves
"park a template" and "instantiate a concrete conform" in module order, and
instantiating a concrete conform can trigger `materialize` through its bound
check. So a target could be materialized before a later module's template is
even parked, which would make the `materialized` memo unsound. Splitting the
loop into *park all templates*, then *instantiate all concrete conforms* fixes
that and removes an existing module-order dependence.

**Cycle guard.** `Conformances::in_progress: Vec<(HirId, ResolvedType)>`,
pushed around the bounds check in `instantiate_conformance`. Re-entry on the
same pair reports `ConformanceCycle` and answers "not satisfied", so the bound
fails cleanly rather than recursing. Recorded in the existing `failed` list so
it is reported once, the same way an unsatisfied bound already is
(`conformances.rs:54`'s doc comment explains that mechanism).

### Risks and open questions

- **A blanket body is only type-checked when instantiated.** A library that
  declares `conform<T: Numeric> T to Sum` and never uses it ships without its
  body ever being checked, C++-template style. This is *pre-existing* — it is
  already true of `conform<T> List<T> to ToIterator<T>` — but blankets make it
  much easier to hit. Out of scope; record in `docs/14-known-issues.md`.
- **A latent ambiguity ships silently.** Two unrelated blankets for one spec
  are only diagnosed when some type satisfies both bounds, so a library can
  publish an overlapping pair that only breaks a downstream user. This is the
  accepted cost of not doing upfront overlap checking; record it.
- **Eviction after a vtable was built.** `Analyzer::type_implements_spec`
  returns the concrete slot list a vtable is keyed on. Registration happens in
  `collect_conformance_signatures`, vtables are built during `check_bodies`, so
  no vtable can be built from an entry that is later evicted. **The executing
  agent must verify this ordering holds** rather than assume it — if a coercion
  can be resolved during signature collection, eviction needs to invalidate the
  vtable cache too.
- **`materialize` is O(templates) on first touch per type.** Acceptable at the
  current scale — the whole tree has 10 generic `conform` templates and 1
  `primitive` template. If it becomes hot, index templates by target head; do
  not do this pre-emptively.
- The user's example spells `sum(self, other: Self)`. Spec functions must take
  `*self` (`SpecSelfMustBePointer`, `docs/08-specs.md:52`), so the real
  signature is `sum(*self, other: *Self) => Self`. No design impact.

## Implementation Plan

Steps 1–3 are pure refactors that must leave every existing test green with no
behaviour change; do not proceed past a red tree.

1. **Introduce the precedence key.** In `omega-driver/src/conformances.rs`, add
   `ConformanceOrigin` and `ConformanceRole` as above plus
   `ConformanceEntry::precedence()`. Replace `ConformanceEntry::from_template`
   with `origin` and `derived: bool` with `role`. Set `origin` in
   `instantiate_conformance` from the classifier added in step 2 (until then,
   `Concrete` when `substitution.is_empty()` else `Generic`). Rename
   `ConformanceOwner::from_template` to `monomorphized` in
   `omega-analyzer/src/checked.rs` and its one consumer at
   `omega-codegen/src/cranelift/item.rs:125`; populate it from `origin !=
   Concrete` in `Driver::conformance_owner`. Replace the three conformance
   checks in `omega-driver/src/compile.rs` (`:649`, `:809`, `:1003`) with
   `origin` comparisons — note `:1003`'s sense is inverted (`!= 1` means
   *concrete*). Leave the primitive ones (`:791`, `:985`) behaving identically.

2. **Unify target admissibility.** Add `Analyzer::is_conformable_target(&
   ResolvedType) -> bool` in `analysis/specs.rs` holding the rule currently
   inline at `specs.rs:148`, and call it from `resolve_conform_target`. In
   `conformances.rs`, replace `template_target_is_matchable` with
   `ConformanceOrigin::classify(target: &Type, generics: &[HirGenericParam])
   -> Option<ConformanceOrigin>`: `None` for a shape nothing can match,
   `Blanket` for a bare unqualified name that is one of `generics`, `Generic`
   for `Type::Generic`/`Type::InferredArray`, `Concrete` when `generics` is
   empty. Keep it adjacent to `match_conform_target` with a doc comment stating
   the two must agree.

3. **Split the conflated diagnostic.** Add
   `AnalysisErrorKind::UnconstrainedConformanceParameter { parameter }` for a
   generic parameter the target never mentions, reusing the second half of
   `blanket_parameter` (`conformances.rs:745`). Delete
   `BlanketConformanceNotYetSupported` and the first half. Update
   `error/kind.rs`, `error/render.rs`, and split the existing test
   `blanket_conformances_are_rejected_with_their_own_diagnostic` into the
   still-rejected case only.

4. **Hoist spec resolution.** Change `Analyzer::check_conform_block` in
   `analysis/specs.rs` to take a resolved `(Rc<RefCell<ResolvedSpecType>>,
   Vec<ResolvedType>)` instead of the raw `&Type`, and have
   `instantiate_conformance` resolve it beforehand with the existing
   `Analyzer::resolve_spec_reference` (already used by `check_generic_bound` at
   `specs.rs:1069`). Reorder `instantiate_conformance` to the sequence in
   *Chosen approach*, still with today's precedence semantics.

5. **Precedence-aware registration.** Replace `reject_duplicate_conformance`
   and the `retain` block at `conformances.rs:493-508` with one
   `register_conformance(&mut self, entry) -> bool` implementing the four-way
   incumbent comparison. Add `AnalysisErrorKind::AmbiguousConformance {
   target, spec, first: Span }`. Blanket-vs-blanket subsumption uses
   `Self::transitive_dependency_ids` on the two declarations' bound specs; the
   bound spec is available from `ConformanceEntry::bounds`' first element.
   Existing behaviour (declared beats derived, duplicate concrete rejected)
   must be unchanged — the existing tests are the guard.

6. **Two-pass collection and `materialize`.** Restructure
   `collect_conformance_signatures` into park-all-templates then
   instantiate-all-concrete. Add `Conformances::materialized: Vec<ResolvedType>`
   and the `materialize` primitive; rewrite `conformance_for`
   (`conformances.rs:619`) and `conformances_for_type` (`:712`) as
   `materialize` + query.

7. **Cycle guard.** Add `Conformances::in_progress` and
   `AnalysisErrorKind::ConformanceCycle { target, spec, declarations:
   Vec<Span> }`. Push/pop around the `check_generic_bounds` call in
   `instantiate_conformance`; on re-entry report once, push to `failed`, return
   `None`.

8. **Enable blankets.** Add the blanket arm to `match_conform_target`
   (`conformances.rs:778`): a bare unqualified target naming one of the
   declaration's generics binds that parameter to `actual`, **provided**
   `is_conformable_target(actual)`. Park `Blanket` templates in
   `collect_conformance_signatures` instead of erroring.

9. **Declaration-time orphan rule for blankets.** Add
   `AnalysisErrorKind::BlanketConformanceForeignSpec { spec_package }`. When
   parking a `Blanket` template, resolve its spec reference and require
   `spec.module_path.first() == module.first()`. Leave
   `check_conformance_orphan` untouched for the other two origins.

10. **Documentation.** `docs/08-specs.md`: a blanket section under
    *Implementing* covering syntax, the specificity rule, the tightened orphan
    rule, and ambiguity. `docs/14-known-issues.md`: the two accepted
    limitations from *Risks*; remove the blanket entry from the "deliberate
    limitations" list. Archive this file as
    `docs/plan/0008-blanket-conformance.md` on completion.

## Testing

**New cases** (`compiler/omega-driver/tests/conform.rs` unless noted):

- Step 1–4 add no tests; the existing 35 must pass unchanged. Treat any
  behaviour diff as a regression, not an improvement.
- *Step 5:* an explicit concrete conform and a matching blanket for the same
  `(target, spec)` compile, and **exactly one** body is emitted for that pair —
  assert over `program.modules[..].items` filtered to
  `CheckedItem::FunctionDefinition` with the matching `conformance_owner`, the
  way `distinct_generic_spec_conformances_emit_distinct_bodies` already does.
  This is what closes the coverage gap `docs/14-known-issues.md:122` records
  for the derived case: unlike derived entries, the losing blanket is *never
  registered*, so the emitted set is discriminating.
- *Step 6:* a blanket instantiated for two different targets emits two distinct
  bodies; instantiating one target twice emits one.
- *Step 7:* `conform<T: A> T to B` + `conform<T: B> T to A` reports
  `ConformanceCycle` and terminates. **Give this test a hard timeout** — a
  regression here hangs the suite rather than failing it.
- *Step 8:* the motivating case end to end — `spec Numeric`-bounded blanket,
  one bound-dispatched call and one `Sum::sum(...)` call, both compile.
- *Step 9:* a blanket whose spec is owned by an `--extern` package reports
  `BlanketConformanceForeignSpec` at the declaration.
- *Runtime proof* (`examples/dev/dev.omg` + `tests/*.expected`): a blanket and
  a specialized conform whose bodies print different values, so `just run-exec`
  shows the specialized body actually ran. The unit tests above prove which
  body is *emitted*; only this proves which one is *called*.

**Negative cases** — each must fail with the named diagnostic, and the message
is part of the deliverable:

- `conform<T, U: Foo> List<T> to Bar` → `UnconstrainedConformanceParameter`,
  naming `U`, labelled at the parameter.
- Two blankets with unrelated bounds both matching one type →
  `AmbiguousConformance`, with a secondary label on the other declaration and a
  note naming the concrete target that made them overlap.
- Blanket + foreign spec → `BlanketConformanceForeignSpec`, with a help line
  saying a blanket can only conform a spec its own package declares.
- Two blankets with *related* bounds (`spec Ord : Eq`) → compiles, `Ord` wins.
- `conform<T: X> T to S` must not bind `T` to a pointer or spec-object target.

**Regression risk** — most likely to break, in order:

1. `just test-multi-print` — the guard for conformance-method linkage. Step 1
   rewrites exactly the field that test exists to protect.
2. `just test-core-only` / `just test-allocator-only` — assert that unused
   conformance paths are *absent* from the link. A blanket that over-matches
   would retain code these tests require to be dropped.
3. The 35 existing `conform.rs` tests, particularly
   `an_explicit_conform_wins_over_a_derived_dependency_entry` and
   `a_bound_on_a_spec_alias_reaches_its_members_conformances`, which pin the
   precedence behaviour step 5 rewrites.
4. Symbol stability: `core`/`std`/`plat` object symbol tables must be
   **byte-identical** before and after, since no runtime code declares a
   blanket. Verify with a baseline build in a detached worktree and
   `diff <(nm --defined-only base.o | sort) <(nm --defined-only new.o | sort)`.

**Target coverage:** `just test-core-only` covers the freestanding,
no-allocator path and must stay clean — blanket conformance must add nothing to
`core.o`, which today has zero relocations.
