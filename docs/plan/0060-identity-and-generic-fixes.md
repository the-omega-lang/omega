# Query identity unification

## Task Description

- **Deliverable:** one item-query identity in `omega-driver` that can name (a) one
  candidate inside an overload group and (b) one declaration inside an owner
  instantiation. The overload signature/body caches, the `MethodKey` query family,
  and their separate whole-program sweeps collapse into the ordinary `ItemKey`
  query path. Delivered as one task in two internally-ordered phases; Phase 2 is
  gated on a normative decision recorded under *Risks* below.

- **Purpose:** `ItemKey { module, name, generic_args }` cannot address two
  declarations that share a module and a name, and cannot address a declaration
  that belongs to an owner rather than a module. Every structure listed in
  *Affected files/symbols* exists only to route around those two facts. The result
  is three parallel query pipelines where the architecture documents one
  (`docs/architecture/module-driver-and-linkage.md`, "Item query identity", states
  the intended end state: "there is no second parallel generic instantiation
  engine for named items"). Consolidating removes duplicate mechanism, restores
  cycle diagnosis to method instantiation, and brings overload handling back into
  conformance with `docs/language/functions.md`.

- **Chosen direction:** extend the existing key rather than introduce a new
  mechanism.

  1. **Disambiguator.** `ItemKey` gains a declaration disambiguator. Use the
     declaration's index within its module's HIR item list, defaulting to the sole
     declaration for an unambiguous name. This is not a new concept: the overload
     caches are already keyed `(ModulePath, usize)` on exactly that index, and
     `Driver::ensure_item_body(key, index)` already threads the same index
     *beside* the key. Folding it into the key removes the parallel parameter.
  2. **Scope.** `ItemKey`'s module field becomes a scope that is either a module
     path or an owner `ItemKey`. A method is then addressable as
     `scope = Owner(owner_key), name = method name, disambiguator = index among the
     owner's declarations`, which is what `MethodKey { owner, method: HirId,
     generic_args }` encodes today in a separate shape.

  The disambiguator is a **query key component only**. It must never reach symbol
  mangling — see *Interfaces/invariants*.

- **Rejected alternatives:**
  - *Keep `ItemKey`/`MethodKey` distinct and make the query state machine generic
    over the key.* Fixes the missing `InProgress` state and the cycle breadcrumb
    with far less churn, but leaves two cache families and two emission loops —
    i.e. it preserves the duplicate mechanism this task exists to remove.
  - *Use the declaration's `HirId` as the disambiguator.* Consistent with
    `MethodKey.method: HirId`, but `ItemQueries::identity_for` mints *synthetic*
    `HirId`s for instantiations, so the key would need a separate rule for which
    id is canonical. The declaration index has no such ambiguity and is already
    the overload path's identity.

## Technical Details

- **Initial context boundary:** `compiler/omega-driver/src/items/`,
  `compiler/omega-driver/src/bodies.rs`, `compiler/omega-driver/src/compile/`,
  `compiler/omega-analyzer/src/resolver.rs`,
  `compiler/omega-analyzer/src/analysis/calls/overload.rs`, and
  `docs/architecture/module-driver-and-linkage.md`. Open
  `docs/language/functions.md` ("Overloading", and the generic paragraph at the
  end of the preceding section) before touching candidate ranking. Do not open
  MIR/codegen except to confirm the mangling invariant below.

- **Affected files/symbols** (all verified present):

  | Site | Change |
  |---|---|
  | `items/mod.rs` — `ItemKey` | gains scope + disambiguator |
  | `items/mod.rs` — `MethodKey`, `MethodQueryState` | removed; absorbed into `ItemKey` / `ItemQueryState` |
  | `items/mod.rs` — `ItemQueries::{method_instantiations, method_bodies, method_identities}` | removed; folded into `item_states`, `generic_instantiations`, `decl_id_owner` |
  | `items/mod.rs` — `overload_signatures`, `overload_bodies` | removed; ordinary item caches serve both |
  | `items/mod.rs` — `identity_for`, `decl_id_owner` | absorb the method-identity mapping |
  | `items/mod.rs` — `failures_retain_a_cause` | now covers what were method states |
  | `items/methods.rs` | `generic_method_template` / `instantiate_generic_method` reduce to owner-scoped key construction over the ordinary query path; `find_generic_method`, `find_generic_conformance_method`, `owner_item_key` stay as template lookup |
  | `bodies.rs` — `ensure_item_body(key, index)` | drops the `index` parameter |
  | `bodies.rs` — `ensure_overload_signature`, `ensure_overload_body` | removed |
  | `compile/signatures.rs:316` and `compile/bodies.rs:46` | the two extra sweeps are deleted; candidates resolve through the ordinary sweep |
  | `compile/signatures.rs` — `check_overload_duplicates` | folded into the module index's ordinary duplicate reporting (`modules.rs::index_items`) |
  | `compile/mod.rs` | the separate method-instantiation emission loop merges with the `generic_instantiations` drain |
  | `resolver.rs:420` — `ModuleResolver` | `generic_method_template` / `instantiate_generic_method` / `resolve_overload_set` are re-expressed over the unified key; the seam itself stays |

- **Interfaces/invariants** — these are settled and must survive unchanged:

  1. **Symbols do not change.** `mangle::function_symbol`
     (`omega-mir/src/mangle.rs`) stamps the full signature — parameters, return
     type, variadic flag, convention (`mangle/semantic.rs:179`) — into every
     function symbol, so overload candidates already have distinct linker names.
     The disambiguator must not enter any `ManglePath`. **This task is not an ABI
     change and must not become one.** Verify by diffing emitted symbols before and
     after (see *Testing*).
  2. **Declaring-module identity.** A concrete instantiation whose template lives
     in an extern package is materialized by the local compilation but filed under
     the *template's* declaring module path, because MIR builds the symbol from
     that path. Owner-scoped keys must resolve to the same declaring module they
     do today.
  3. **Determinism.** Any cache whose iteration order can reach synthetic IDs,
     emitted item order, diagnostics, or symbols stays an insertion-ordered map.
     `item_states`, `generic_instantiations`, and `cells` are `IndexMap` today and
     must remain so after absorbing the method caches.
  4. **Two-phase split.** Signatures resolve before bodies are checked. Removing
     the extra sweeps must not move candidate signature resolution into the body
     phase.
  5. **Body-after-signature.** A body is checked only for an item whose signature
     reached the resolved state; a lazily discovered generic body is checked only
     after its signature completed. Both are listed under "Maintainer invariants
     that are easy to break" and both still apply to owner-scoped keys.
  6. **A failed body never demotes its own resolved signature.** A failed
     *signature* stays `Failed`, so later call sites reference one reported
     error. A failed *body* must leave the signature `Resolved`: the declaration
     still has a usable signature, and demoting it makes every call site emit a
     derived "because of its own error" secondary on top of the real diagnostic.
     This was implemented and reverted once during Phase 1 (an owner-scoped
     `fail_body` that made methods emit 1 + N diagnostics where free functions
     emit 1), so it is recorded here rather than left as a shape someone
     re-derives. Owner-scoped and module-scoped keys must not diverge on this.
     Covered by `generic_methods.rs::a_failed_method_body_reports_once_like_a_free_function`
     beside the signature-failure test above it.
  7. **`ModuleResolver` stays the seam.** The analyzer must keep asking the driver
     for candidates; do not move overload-set construction into the analyzer.

- **Out of scope:**
  - The `meet` / `primitive` block restriction on generic declarations. It is
    caused by eager signature collection in those blocks, **not** by the key, and
    it is not fixed by this task.
  - Generic `foreign` function monomorphization. It is commonly grouped with this
    cluster, but it has its own unresolved FFI question (per-instantiation mangled
    symbols versus one shared external symbol) that this key change does not
    answer. Leave the `GenericForeignFunctionUnsupported` rejection in place.
  - Interning module paths / splitting `ModulePath` from item paths.
  - Narrowing the `ModuleResolver` facade.
  - Any change to emitted symbols, linkage, or object layout.

- **Risks/open questions — escalate, do not decide alone:**

  **Phase 2 amends an internally inconsistent normative rule and cannot begin
  without the user's explicit approval of the new wording.**
  `docs/language/functions.md` (generic paragraph preceding "Overloading") draws
  two consequences from "a generic declaration has no signature until a call
  determines its arguments": that it cannot be named uncalled, and that it "does
  not participate in overload resolution... since nothing can rank them".
  `docs/language/generics.md` (end of "Generic member and static functions") restates the second.

  The first consequence is sound: an uncalled reference supplies no arguments, so
  nothing can drive monomorphization and there is no signature to match against an
  expected function type. The second does not follow, because overload resolution
  happens *at the call*, where the arguments do exist — and `generics.md`
  ("Function type inference", "Defaults and function-call inference") already
  normatively specifies the algorithm that solves a declaration's generic
  arguments from exactly those call arguments. A candidate whose parameters solve
  has a concrete signature, which the existing minimum-cost rule ranks like any
  other. The two chapters therefore disagree, and "nothing can rank them" is true
  only of the uncalled position.

  Phase 2 is consequently a **clarification**: split the rule so that an uncalled
  reference stays excluded while call-site candidates participate through ordinary
  inference. It is still a normative edit to two chapters and still requires
  approval.

  **How that approval works — do not block on it before starting.** The rule
  itself is already decided and is written out in step 9; what needs approval is
  the *chapter text*, which does not exist yet and therefore cannot be approved
  in advance. Draft the `functions.md` and `generics.md` amendments as part of
  step 11, then stop and present them for review. Treating approval as a
  precondition means reporting blocked without having produced the thing being
  approved.

  Phase 1 contains **no** language change. It does, however, correct a genuine
  specification divergence: today the sweep at `compile/signatures.rs:316`
  resolves every overload candidate's signature unconditionally, so a
  generic/non-generic pair fails **with no call site present**, whereas the
  specification says such declarations are rejected *where they are called*.

## Implementation Plan

Each step should leave the tree building and the conformance suite passing.

1. **Fold the declaration index into `ItemKey`.** Add the disambiguator field,
   update construction sites, and drop the separate `index` parameter from
   `ensure_item_body`. No behavior change yet: every existing key uses the sole
   declaration's index.

2. **Route overload candidates through the ordinary query path.** Delete
   `ensure_overload_signature` / `ensure_overload_body` and the
   `overload_signatures` / `overload_bodies` caches; have `resolve_overload_set`
   build one ordinary `ItemKey` per candidate. Delete the sweeps at
   `compile/signatures.rs:316` and `compile/bodies.rs:46` together with the
   `is_overloaded` special case below them.

   **`check_overload_duplicates` stays in the signature phase — do not move it
   into `modules.rs::index_items`.** It compares *resolved* parameter types
   (`a.param_types().eq(b.param_types())`, `bodies.rs:372`), which do not exist at
   indexing time; `index_items` compares claimed names only, and letting two
   same-named functions collide there is exactly the overload exception it
   deliberately makes. What disappears is its dedicated *sweep*, not the check —
   it runs over the ordinary per-name query results instead of a separate pass.

3. **Confirm the specification divergence is closed.** *(Delivered — recorded
   here because the delivered behavior differs from this step as first written.)*
   With candidates on the ordinary path, a generic candidate is an uninstantiated
   template with no eager signature, so a generic/non-generic pair compiles
   rather than failing at signature collection.

   Beyond that, `raw_overload_signatures` **skips** a generic candidate instead
   of failing the group. `functions.md` excludes a generic declaration from
   overload resolution, so a concrete candidate of the same name still wins —
   which is what the member path already did. Only a group whose candidates are
   *all* generic has nothing to rank, and that is the case the chapter rejects at
   the call. The first draft of this step directed the opposite (fail the whole
   group on any generic candidate); that was a misreading of the chapter and
   would have left top-level and member behavior diverging.

4. **Generalize the key's scope.** Replace `ItemKey`'s module path with a scope
   that is either a module path or an owner `ItemKey`. Keep `MethodKey` alive as a
   type alias during this step if it shortens the diff, but do not leave it in the
   final tree.

5. **Absorb the method query family.** Move `method_instantiations` into
   `item_states`, `method_bodies` into `generic_instantiations`, and
   `method_identities` into `decl_id_owner`. Re-express
   `generic_method_template` / `instantiate_generic_method` over owner-scoped
   keys. Merge the method emission loop in `compile/mod.rs` into the
   `generic_instantiations` drain, preserving the declaring-module rule
   (invariant 2).

6. **Give method instantiation a real `InProgress` state.** `MethodQueryState`
   disappears into `ItemQueryState`, so owner-scoped queries gain `InProgress`, a
   `resolution_stack` entry, and `failures_retain_a_cause` coverage.

   **Ordinary recursion must keep working.** A recursive call that finds an
   already-*completed* signature resolves normally, exactly as it does on the item
   path. `tests/t10c_generic_member_functions` exercises this directly —
   `repeat<B>` calls `self.repeat(depth - 1, b)` at the same generic arguments and
   must continue to compile and run. This step must not turn self-recursion into
   an error.

   What changes is only re-entry into a signature that is *still being resolved*.
   Today nothing models that state: the resolved signature is inserted into
   `method_instantiations` before `check_function_body` runs, so the invariant
   rests on statement order, and a reordering or a new early return between those
   two statements converts re-entry into unbounded recursion that hangs. With
   `InProgress`, that window becomes a diagnosable state with a `resolution_stack`
   chain to name, matching how the item path already behaves. Remove the ordering
   comment at the insertion site; the state replaces it.

   Note that this is a **fragility** fix, not a live source-level bug:
   `design-debt.md` describes the hazard as ordering-dependent, and it may not be
   reachable from any Omega source today. Do not invent a contrived source case to
   force it — see *Testing*.

7. **Update the architecture documentation.** In
   `docs/architecture/module-driver-and-linkage.md`: rewrite "Item query identity"
   for the unified key, delete the `MethodKey` block, and replace the maintainer
   invariant that currently reads as "the signature is cached before the body is
   checked, so a declaration that instantiates itself finds it rather than
   re-entering" — that ordering convention is now an explicit state. Keep the
   declaring-module and determinism invariants.

8. **Record one new limitation, then retire the resolved entries.**

   Add an entry to `docs/issues/language-limitations.md` under `## Functions`:
   an expected function type is sufficient information to determine a generic
   declaration's arguments, and Omega already uses an expected function type to
   select among *overloads* at an uncalled reference
   (`f : (thing: u32) => void = print_any;` in `functions.md`), but it does **not**
   use it to instantiate a generic — `fnptr : (i32) => void = some_generic_function;`
   is rejected by `GenericFunctionNotInstantiated`. Record it as a deliberate
   current restriction, not a bug, and note that the underlying invariant is
   "no signature until some context determines the arguments" rather than "no
   signature until a call" — a call, an expected result type, and explicit
   arguments are all already-accepted determining contexts, so the exclusion is
   about which contexts are *wired up*, not about what is knowable. Do not amend
   `docs/language/functions.md` for this; the normative rule stands as written.

   **A separate, approved `functions.md` addition did land in Phase 1**, and is
   not a breach of the line above: the "Overloading" section gained
   "Adapting a literal is a last resort, not a default" with a worked
   `f(10)`/`f(10i64)` example. It was requested directly by the user, it
   describes behavior the compiler already had (verified against emitted IR —
   `f(10)` calls the `i32` candidate, `f(10i64)` the `i64` one), and it changes
   no semantics. It is a prerequisite for Phase 2's ranking rule, which builds
   on adaptation cost being the outer gate. Do not revert it as an out-of-scope
   edit.

   Then remove from `docs/issues/design-debt.md`
   the entries "Overloading is a second, parallel item pipeline…", "Generic
   member/static instantiation is a third query identity…", and "Method
   instantiation's re-entrancy guard is an ordering convention…", and the
   corresponding summary bullets in `docs/issues/known-issues.md` under "Compiler
   internals". Do **not** remove the generic-overload limitation entries in
   `docs/issues/language-limitations.md` unless Phase 2 is approved and completed.

**Phase 2 — only with approved amendment of `docs/language/functions.md`:**

Phase 1 is complete, so Phase 2 starts from the delivered behavior in step 3,
not from the pre-Phase-1 tree.

Phase 1 is committed as `885bae2 "implement phase 1 of fixes"`, including both
post-review fixes and `tests/t05c_generic_overload_declarations/expected.stdout`.
Phase 2 therefore starts from a clean tree.

Two consequences for the existing cases:

- `t05c` (mixed group, concrete wins) must keep passing **unchanged**. Under
  Phase 2 the concrete candidate still wins that call — both candidates reach
  zero adaptation cost and specificity breaks the tie — so any change to its
  output means the ranking rule was implemented wrongly. It is the best
  regression anchor available for step 10.
- `t05b` (all-generic group) changes meaning and needs a regenerated
  `expected.stderr`. Once templates are ranked, its two candidates both deduce
  successfully and both are unbounded, so the call becomes an **ambiguity**
  rather than "declared generic more than once".

Decide as part of step 11 what remains of `ResolveError::GenericFunctionOverload`
and `GenericMethodOverload`. Once an all-generic group is rankable, the condition
they describe is largely replaced by the ambiguity diagnostic; leaving both in
place would be two mechanisms for one outcome.

9. Define candidate ranking by **reusing Omega's existing specificity rule**, not
   by inventing one for functions. `docs/language/specs-and-conformance.md`
   (selection rules 1-5, immediately after the blanket-conformance example)
   already states "more specific wins" normatively, and
   `Driver::compare_conformance_precedence`
   (`omega-driver/src/conformances/registration.rs:513`) already implements it:
   concrete beats generic by origin; two generic candidates compare by
   alias-expanded declared bound-key subset; neither-subsumes is incomparable and
   therefore ambiguous; an unbounded candidate has the empty bound set and loses
   to any otherwise-matching bounded one.

   Mapped onto overloads: a concrete declaration beats any generic one
   (`thing(a: i32)` wins over `thing<T>(a: T)`), two generic candidates compare by
   bound sets, and incomparable sets are ambiguous. Note that this rule ranks by
   concreteness and bounds **only** — not by parameter structure. `f<T>(x: T)`
   versus `f<T>(x: *T)` are both unbounded, so they are ambiguous rather than
   `*T` winning. Do not add structural specificity; that would be a second,
   divergent notion of "more specific".

   **Layering is settled: adaptation cost gates, specificity breaks ties.**
   `functions.md` already selects the unique minimum-cost candidate, counting
   literal-adaptation cost. That rule stays the outer gate, and "more specific
   wins" orders only the candidates that tie at the minimum. Specificity is **not**
   an outer tier, because a conversion is a last resort rather than a default:

   - `f(10)` against `{ f(a: i64), f<T>(a: T) }` selects the **generic**. `10` is
     `i32`, so the generic deduces `T = i32` at zero adaptation cost while the
     concrete candidate would require converting to `i64`.
   - `f(10i64)` against the same pair selects the **concrete** one. Both match at
     zero cost — `f(a: i64)` exactly, `f<T>(a: T)` by deducing `T = i64` — so the
     tie is broken by specificity.
   - `f(10)` against `{ f(a: i64) }` alone converts, because there is no
     alternative.

   This is the same shape as conformance selection, where rules 1-5 order the
   *matching* conformances and matching is the gate — not a second ordering
   principle layered above it.

   Two subsidiary rules, both consequences of the above rather than new choices:

   - **Bounds are checked after selection, not during ranking.** The existing
     comparator compares declared bound *keys* structurally and never proves them,
     so ranking stays solver-free and the winner's bounds are then checked
     normally. This also avoids running the goal-directed conformance solver
     speculatively, which `module-driver-and-linkage.md` warns against: only an
     outermost goal's failure is safe to memoize permanently.
   - **Defaults never create viability.** A declared generic default applies only
     after a candidate is selected; otherwise `add<T = u64>(a: T, b: T)` is
     trivially viable against almost any argument list.
   - **Duplicate detection must be extended to templates.** `check_overload_duplicates`
     compares resolved parameter types, which a template does not have, so two
     identical generic declarations (`thing<T>(a: T)` and `thing<U>(a: U)`) would
     otherwise be accepted and then be ambiguous at every call. Follow the
     conformance precedent, where two identical blankets are a declaration-time
     `DuplicateConformance`: compare templates structurally (declared parameter
     types plus bound-key sets) and report `Redeclaration` at the declaration, not
     at the call.
   - **An uncalled reference still excludes generic candidates.** Settled: an
     expected function type continues to select among *overloads* only, and never
     instantiates a generic. `functions.md`'s "cannot be named without being
     called" stands unchanged. The resulting asymmetry is recorded as a tracked
     limitation in step 8 rather than fixed here.

10. Rank generic candidates in `analysis/calls/overload.rs` by unifying against
    the **raw declared shape**, instantiating only the winner.

    **Do not use `Analyzer::without_diagnostics` as the isolation mechanism.**
    An earlier draft of this step named it; that was wrong. It truncates the
    analyzer's own `errors`/`warnings` vectors
    (`analysis/mod.rs:629`) and cannot undo driver-side effects — a memoized
    `ItemQueryState::Failed`, a diagnostic already recorded in the driver's sink,
    or a body checked as a side effect. A losing candidate must not leave any of
    those behind.

    The isolation comes from **not starting speculative driver work at all**,
    which the existing inference path already makes possible:

    - `ModuleResolver::generic_function_signature` returns a `GenericSignature`
      whose `params` are `Vec<Type>` — *syntactic*, not resolved. The
      architecture doc calls these "focused raw-signature queries that do not
      instantiate the item just to inspect its generic pattern"
      (`module-driver-and-linkage.md`, "Generic inference vs instantiation").
    - Analyze the call arguments **once**, before ranking, exactly as concrete
      overload resolution already must. Arguments are real caller expressions;
      analyzing them is not speculative and must not be repeated per candidate.
    - Per candidate, unify its syntactic `params` against the already-analyzed
      argument types. This is a structural operation producing a candidate
      substitution — see `infer_generic_args` in `analysis/calls/generic.rs`,
      which is the same unification an ordinary generic call already performs.
    - Instantiate only the winner, through the ordinary `ItemKey` path, then
      check its bounds.

    **The line to hold:** resolving names *written in a candidate's own
    signature* is non-speculative — a declaration naming an unresolvable type is
    a real error belonging to that declaration, whichever overload wins.
    *Instantiating anything under a trial substitution* is speculative and is
    forbidden during ranking. Ranking must also not prove conformances (bounds
    are checked after selection, per step 9) and must not check bodies.

    If a case is found that genuinely cannot be ranked without starting
    speculative driver work, **stop and escalate rather than improvising**. The
    correct mechanism would be a driver-level savepoint that buffers query-state
    and diagnostic mutations until committed — a new driver capability with its
    own design, not something to approximate with local suppression. Note that
    `comp_param_types` already wraps `without_diagnostics` around a path that can
    reach the driver; that is pre-existing and out of scope here, but do not
    treat it as precedent for this step.

11. Amend `docs/language/functions.md` and `docs/language/generics.md`. The
    amendment should **cite** `specs-and-conformance.md`'s selection rules rather
    than restating them, so Omega keeps one written definition of "more specific".
    Remove the now-resolved entries from `docs/issues/language-limitations.md`
    ("A generic member/static declaration does not participate in overload
    resolution").

    **Drafts already exist and have been reviewed.** A draft of both chapters was
    written before step 10 was attempted. It is substantively correct — the split
    rule, specificity breaking minimum-cost ties via the conformance rules,
    defaults not establishing viability, bounds checked after selection, no
    parameter-structure specificity, template redeclaration reported at the
    declaration, and uncalled references excluding generic candidates all match
    the decisions in step 9. The `#blanket-conformances` anchor it links to is
    valid. Do not redo that work; apply the corrections below and submit for
    approval.

    **Required corrections — implementation vocabulary in normative chapters.**
    A language chapter states what Omega *means*, not how the compiler computes
    it. Three phrases leak the latter:

    - `functions.md`: "alias-expanded declared **bound-key** sets".
      `declared_bound_keys` is a driver field name. `specs-and-conformance.md`
      says "required bound sets"; match that chapter's vocabulary, since the
      whole point is to cite one definition rather than fork it.
    - `generics.md`: generic declarations participate "through **speculative
      inference**". That names an implementation technique. They participate
      under this chapter's ordinary inference rules; whether the compiler probes
      speculatively is not a language fact.
    - `functions.md`: "**Probing** an unsuccessful candidate produces no
      diagnostics." The observable rule is worth stating and should be kept, but
      phrase it as a property of the candidate — a candidate that is not viable
      produces no diagnostics of its own — rather than of the probe.

## Testing

- **Regression coverage (the primary gate for Phase 1):** `tests/t05_functions`
  (overloading), `tests/t10_generics`, `tests/t10c_generic_member_functions`
  (`repeat<B>` recurses at the same generic arguments and **must keep
  compiling and running** — this is the case that hangs if the insertion ordering
  is disturbed, so it is the direct proof that step 6 preserved recursion),
  `tests/t10d_generic_member_function_errors`, `tests/t29_type_function_namespaces`
  and `tests/t29b_type_function_namespace_errors` (the two associated-function
  overload domains), `tests/t26_aliases` (overloaded-name aliasing),
  `tests/t36_multi_object` (separate compilation).

- **Symbol invariance (mandatory, invariant 1):** before starting, capture the
  emitted symbol set for `runtime/core`, `runtime/std`, and `tests/t36_multi_object`
  (`omgc --emit=ir`, or `nm` over the built objects). After step 6, the sets must
  be byte-identical. A diff here means the disambiguator leaked into mangling and
  the change has silently become an ABI migration — stop and escalate.

- **New component tests** (`compiler/omega-driver/tests/`, black-box):
  - a failed owner-scoped instantiation is reported once and later call sites
    reference it rather than repeating it (the `failures_retain_a_cause` property,
    now extended to owner-scoped keys).

  For the `InProgress` state itself, prove the **state machine**, not a source
  program: assert that re-entering an owner-scoped key that is currently
  `InProgress` yields a cycle result carrying the `resolution_stack` chain, in the
  same shape the item path already uses. If no Omega source can reach that window
  today, that is the expected outcome and not a reason to manufacture one — the
  step exists to make the invariant structural rather than ordering-dependent.
  `tests/t10c_generic_member_functions` remains the regression proving ordinary
  recursion still compiles and runs; it must pass unchanged.

- **Conformance cases — delivered as two, not one.** `tests/t05b_generic_overload_errors/`
  is the negative case: an **all-generic** group (`free<T>(ptr: *T)` beside
  `free<T>(ptr: T)`), whose call has nothing to rank, with `expected.stderr`
  pinning the diagnostic text — a compile failure alone is not evidence here.
  `tests/t05c_generic_overload_declarations/` is the positive case: a **mixed**
  group whose call selects the concrete candidate, proved by `expected.stdout`
  rather than by compiling alone, so it shows *which* body ran. **Specification
  trace:** `docs/language/functions.md`, the generic paragraph preceding
  "Overloading".

- **Phase 2 conformance case** (only if approved): positive case proving a
  concrete candidate wins over a viable generic one, plus a negative case proving
  two equally-viable generic candidates are ambiguous. Specification trace: the
  amended "Overloading" section.

- **Commands:** `./bin/test-runner t05_functions t10_generics
  t10c_generic_member_functions t10d_generic_member_function_errors
  t29_type_function_namespaces t36_multi_object` during development; `just
  test-all` as the gate before review.
