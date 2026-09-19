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
  6. **`ModuleResolver` stays the seam.** The analyzer must keep asking the driver
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
  `docs/language/generics.md:214` restates the second.

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
  approval before implementation.

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

3. **Confirm the specification divergence is closed.** With candidates on the
   ordinary path, a generic candidate is an uninstantiated template with no eager
   signature, so a generic/non-generic pair must now compile until it is *called*,
   and the call must report a real diagnostic rather than an unresolved type
   parameter. Prove this with the negative case described in *Testing*.

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

   Then remove from `docs/issues/design-debt.md`
   the entries "Overloading is a second, parallel item pipeline…", "Generic
   member/static instantiation is a third query identity…", and "Method
   instantiation's re-entrancy guard is an ordering convention…", and the
   corresponding summary bullets in `docs/issues/known-issues.md` under "Compiler
   internals". Do **not** remove the generic-overload limitation entries in
   `docs/issues/language-limitations.md` unless Phase 2 is approved and completed.

**Phase 2 — only with approved amendment of `docs/language/functions.md`:**

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

10. Make generic-argument inference speculative in
    `analysis/calls/overload.rs`: probe each candidate, drop a candidate whose
    parameters do not solve, and instantiate only the winner. Failure during
    probing must emit no diagnostics — audit every diagnostic path reachable from
    inference. `Analyzer::without_diagnostics` (already used this way by
    `comp_param_types`) is the existing mechanism; do not add a second one.

11. Amend `docs/language/functions.md` and `docs/language/generics.md:214`. The
    amendment should **cite** `specs-and-conformance.md`'s selection rules rather
    than restating them, so Omega keeps one written definition of "more specific".
    Remove the now-resolved entries from `docs/issues/language-limitations.md`
    ("A generic member/static declaration does not participate in overload
    resolution").

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

- **New conformance case — `tests/t05b_generic_overload_errors/`** (negative,
  Phase 1): a module declaring both `free(ptr: *u8) => void` and
  `free<T>(ptr: *T) => void`. With `expected.stderr`, prove that
  (a) the declarations alone do not error, and (b) a call reports a real
  diagnostic about generic candidates not participating in overload resolution —
  not `unresolved type parameter`. **Specification trace:**
  `docs/language/functions.md`, the generic paragraph preceding "Overloading"
  ("rejected where they are called"). A compile failure alone is not sufficient
  evidence here; the expected diagnostic text is the whole point of the case.

- **Phase 2 conformance case** (only if approved): positive case proving a
  concrete candidate wins over a viable generic one, plus a negative case proving
  two equally-viable generic candidates are ambiguous. Specification trace: the
  amended "Overloading" section.

- **Commands:** `./bin/test-runner t05_functions t10_generics
  t10c_generic_member_functions t10d_generic_member_function_errors
  t29_type_function_namespaces t36_multi_object` during development; `just
  test-all` as the gate before review.
