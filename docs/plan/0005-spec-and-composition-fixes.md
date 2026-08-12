# Composition and specs: structural repair

## Task Description

- **What is being asked:** Fix every open spec/composition issue in
  `docs/14-known-issues.md` — twelve under Composition, four under Specs — and the two
  compiler-internals items they touch. Not as sixteen patches: the tracked issues collapse
  into **six root causes**, and each is fixed once, at its root.

  | # | Tracked issue | Root cause |
  |---|---|---|
  | 1 | Bound context widened to every compose on the type | **D** bounds can't reach their composes |
  | 2 | Generic-target compose at two `T` collides in codegen | **C** instantiations share one identity |
  | 3 | `core`'s spec methods are inherent, composes empty | **A** conformance has two implementations |
  | 4 | Registry keys on `ResolvedType` equality | **B** lookup key finer than identity |
  | 5 | Variant-narrowed enum never finds its compose | **B** |
  | 6 | `primitive<T> [?]T` method on a mutable slice panics | **B** + **C** |
  | 7 | Symbols embed raw type renderings (`*mut [?]u8`) | **E** no target model |
  | 8 | Slice/pointer targets unreachable two ways | **E** |
  | 9 | Extra overload in a compose silently dropped, unchecked | **F** block contents unchecked as a unit |
  | 10 | `hidden` method widened to public through a compose | **A** |
  | 11 | `ComposeTargetNotAType` never constructed | **E** |
  | 12 | Six stale `for`-attached-spec comments | — |
  | S1 | `spec *T` coercion missing in three positions | — |
  | S2 | No `is_variadic` on spec functions | — |
  | S3 | `spec T` return inference unsupported on methods | — |
  | S4 | `ToIterator<T>` twice: no disambiguation | — |
  | I1 | Two independent pending-spec-method queues | **A** |
  | I2 | Only `core` may declare `primitive` blocks | *not a defect — see below* |

- **Purpose:** The composition rework landed its structure but not its guarantee. The
  property `compose` exists to provide — a composed method is reachable only through its
  spec — is not enforced today, and three separate mechanisms independently defeat it. Two
  of the tracked issues are hard compiler panics on ordinary code. Two more emit symbols
  containing characters the mangling scheme deliberately excludes.

  Serves *no hidden behavior* (a receiver call resolves against the type's own declaration
  or a bound you wrote), and *abstractions that compile away* (conformance becomes one
  registry lookup instead of a signature search over method lists).

- **Reasoning:** Fixing these individually would produce sixteen local patches over a model
  that still has two conformance implementations, two identity schemes, and three target
  shapes. The root-cause grouping is not editorial — each cause has exactly one place it is
  decided, and every issue under it disappears when that place is corrected:

  **A. Conformance has two implementations.** `check_compose_block` falls back to the
  target's *inherent* methods when a compose omits a requirement (`specs.rs`), and
  `type_implements_spec` keeps a signature-matching search beside the registry. That
  fallback is why `core` can ship `primitive bool { fmt(...) }` plus `compose bool :
  Display {}`, which is why `42.fmt(w)` is not rejected (#3); it is also what relabels a
  `hidden` method with the spec's visibility (#10). Removing it makes the registry the
  single source of truth.

  **B. A type's lookup key is finer than its identity.** `ResolvedType`'s derived equality
  distinguishes an enum's `variant` and pointer/slice mutability — refinements that are not
  part of "which methods does this type have." Every *existing* lookup path already erases
  them (`find_methods` reads the enum cell's `functions` ignoring `variant`;
  `adapt_self_argument` re-stamps mutability); only the three registry probes compare them.

  **C. A generic instantiation has no identity of its own.** Compose and primitive
  instantiations inherit `decl_id` from the template's HIR, so two instantiations collapse
  onto one symbol at emission. Generic *items* already solved this —
  `ItemQueries::identity_for` mints a fresh synthetic id per `ItemKey` and both phases read
  it back. Composes simply never adopted it.

  **D. A bound cannot reach the composes that satisfy it.** `compose Wolf : Mammal`
  registers under `Mammal` only, so a `T: MySpec` bound (where `spec MySpec = Dummy |
  Mammal`) finds nothing. The current code compensates by seeding *every* compose on the
  concrete type, which is what voids the coherence guarantee (#1).

  **E. There is no target model.** The contextual-keyword lookahead admits `Ident | Lt`
  while `parse_compose_def` calls the full `parse_type`; `match_compose_target` handles only
  `Type::Generic`; `mangle.rs` falls back to `Display`; `primitive_target_allowed` and the
  compose path disagree about what a target may be. Four ad-hoc answers to one question.

  **F. A compose block's contents are not checked as a unit.** The extra-function guard
  tests the requirement *name*, so an extra overload of a required name is accepted, never
  type-checked, and never emitted (#9).

  Alternatives considered:

  - *Patch each issue where it surfaces.* Rejected: #5 and #6 have the same cause and
    different symptoms (a miss vs. a panic); patching them separately guarantees the third
    instance of **B** is found in production.
  - *Route registry lookup through the subtyping/coercion relation* instead of a canonical
    key. Rejected: coercion semantics do not belong in a registry probe, and the two
    refinements are not coercions of the same kind — an enum variant is a proof carried in
    the type, mutability is a qualifier.
  - *Keep the inherent fallback and forbid visibility widening.* Rejected on the user's
    decision, and it would leave the spec-namespace property permanently partial: any type
    declaring a method inherently exposes it on a bare receiver regardless of bounds.
  - *Expand bounds into composes at each lookup site* rather than registering the closure.
    Rejected: expansion would have to be repeated at bound seeding, `type_implements_spec`,
    `for..in`, and spec-qualified calls, and each omission is a silent miss.

- **Resolved concerns:**
  - **The inherent fallback is removed** (decided). A compose supplies every requirement
    itself. Measured cost: ~25 method bodies move from `core`'s `primitive` blocks into
    their compose blocks — `numerics` is macro-generated, so three edits cover twelve
    scalar types — and `std::hash_map`'s five duck-typed `key.equals`/`key.hash` calls
    become spec-qualified. Only 15 receiver-style calls to spec-derived methods exist in the
    whole tree.
  - **Slice targets are supported** (decided): both `compose [?]u8 : Eq` and `compose<T>
    [?]T : Eq`. **Pointer targets stay rejected** — a pointer is structural, not a nominal
    type that can own conformance — but with `ComposeTargetNotAType` rather than a parse
    error about a missing `':'`.
  - **The four `## Specs` gaps are in scope** (decided), including S3, which is the riskiest
    item here and is deliberately sequenced last and alone.
  - **I2 is not a defect.** "Only `core` may declare `primitive` blocks" is the design:
    `primitive` is a *declaration site* for built-in types, and `core` owns them. Any
    package may still compose a spec onto a primitive under the orphan rule. The tracker
    entry should be reworded from a limitation to a stated rule, not fixed.
  - **Symbols change** for `core::strings` and `core::slices` methods on unnamed targets.
    Omega commits to a stable ABI, so this is called out rather than absorbed: the current
    symbols contain `*`, `[`, `]` and a space, cannot round-trip through `omg_demangle`, and
    are not portable. There is no compatible fix; the baseline is re-captured at that step.
  - **`for..in` gains an optional binding type annotation** (`for x : u64 in y`) to resolve
    S4. This is new grammar, but it is the minimum that can disambiguate: `to_iterator`
    takes no arguments, so there is no argument shape to resolve against, and the old
    `<spec *ToIterator<u64>>expr` escape hatch stopped applying when `ToIterator<T>` became
    not-object-safe.

## Technical Details

### The six fixes

**A — one conformance implementation.** Delete `check_compose_block`'s `inherent` parameter
and its fallback arm; delete `type_implements_spec`'s signature-matching search, leaving a
registry lookup that returns the entry's `methods`' `decl_id`s as vtable slots. A compose
that omits a requirement is `MissingSpecFunction` unless the spec supplies a default body.
`SpecMethodTooHidden` stays deleted and #10 disappears with the fallback — there is no
inherent method left to relabel.

**B — a canonical lookup key.** One function, `ResolvedType::lookup_key()`: widen
`Enum { variant: Some(_) }` to `None`, normalize `Pointer`/`Slice`/`Str` mutability to
immutable, everything else identity. Applied at **both** registration and lookup in
`instantiate_compose`, `instantiate_primitive`, `compose_for`, `composes_for_type`, and
`primitive_methods`. Registration and lookup using the same function is the invariant; a
future refinement added to `ResolvedType` must extend this function or it silently
reintroduces #5.

**C — per-instantiation identity.** `ComposeEntry` and `PrimitiveEntry` gain a
`method_ids: Vec<HirId>` decided once at instantiation via `ItemQueries::identity_for`,
keyed on `(declaration HirId, lookup_key(target))` — mirroring `compute_item`'s existing
`method_identities` discipline exactly, rather than inventing a second scheme.
`check_compose_block` stamps those ids instead of reading `function.id`, so two
instantiations of one template are genuinely distinct functions to MIR and codegen.

**D — dependency-closure registration.** Registering `compose T : S` also registers a
derived entry for every spec in `S`'s transitive `dependencies` closure, with each
dependency's type arguments resolved through `flatten_spec_into`'s existing deferred
resolution (a dependency carries **raw** args by design — `spec Foo<T> : Bar<T>` — so a
plain walk is not enough). Derived entries are flagged: `DuplicateCompose` fires only on
directly-declared ones, and diagnostics anchor at the real declaration.

With the closure present, `check_generic_bounds` seeds **exactly** the declared bound and
nothing else, which closes #1. `spec *Animal` coercion of a `Wolf` composed only with
`Mammal` then resolves through the registry, which is what makes A's deletion safe.

**E — one target model.** A `ComposeTarget` abstraction owns the whole question: what may be
written, how it is matched against a concrete type, and how it is mangled.

- Parser: widen the `compose`/`primitive` lookahead from `Ident | Lt` to also admit `[`, so
  the dispatcher can reach every shape `parse_type` accepts. Anything the target model
  rejects then gets a real diagnostic instead of a binding-parse error.
- `match_compose_target` gains the `[?]T` shape alongside `Type::Generic`, so a generic
  slice target binds instead of being silently dropped.
- `ComposeTargetNotAType` is finally constructed, for pointer/array/spec/function targets.
- Mangling encodes structural targets through the existing `MangleType` grammar that
  `mangle_type` already produces for every other position, replacing
  `ManglePath::Root(target.to_string())`.

**F — check the block as a unit.** The extra-function guard matches on `(name, signature)`,
the same pairing the requirement loop directly above it already uses.

**I1 — one pending-method queue.** `ItemQueries::pending_spec_methods` (keyed by `ItemKey`)
and `ComposeEntry::pending` are the same concept with different owners. With conformance
living only in the registry, aggregates no longer queue anything, so the `ItemKey`-keyed
queue can be deleted outright rather than unified.

### The four spec gaps

- **S1**: wire `coerce_to_expected` into the three positions that lack it, verified empirically
  — bare tail return (via `check_function_return`/`block_type`), struct-literal field, and
  array-literal element. Explicit `return`, local declarations, call arguments and assignment
  are already wired and must stay unchanged.
- **S2**: `is_variadic` on `HirSpecFunction`/`RawSpecFunctionSig`, threaded into the
  `ResolvedFunctionType` that `resolve_raw_spec_fn_type` builds (hardcoded `false` today).
- **S3**: `spec T` return-type inference for methods. `collect_methods` passes
  `return_type_override: None` unconditionally; making it work needs
  `resolve_spec_return_function`'s phase inversion to run for a method **while the owning
  type's cell is still `InProgress`**. This is the highest-risk item in the plan.
- **S4**: optional binding type annotation on `for..in` (`ForInStmt::binding` gains a
  `Option<Type>`), used by `classify_for_in_source` to select among multiple
  `ToIterator<T>` entries. Absent annotation with more than one candidate becomes an
  ambiguity error naming each `T`, instead of today's silent first-match.

### What must not change

- **The orphan rule** (target-or-spec-local, package granularity) and its diagnostic.
- **Spec-namespace resolution.** A receiver call still resolves against inherent methods
  plus bound-context composes, never all composes. This plan *narrows* the bound context; it
  must not widen any other lookup to compensate.
- **`flatten_spec`'s ordering and dedup**, which remains the single source of vtable slot
  order.
- **`gap`/`glue`** — a separate mechanism. No glued symbol may move.
- **Enum refinement semantics** (`docs/05`): construction and match narrowing keep working
  exactly as documented. Only *lookup* widens.
- **Package layout.** Nothing moves between `core` and `std`; that remains its own plan.
- **Blanket composes stay out of scope.** `compose<T: Bound> T : Spec` keeps its
  `BlanketComposeNotYetSupported` diagnostic. Note the target model (E) must not
  accidentally make a bare-parameter target matchable.

### Chosen approach

Fix root causes in dependency order, each step buildable and gated by the existing
end-to-end tests. Two orderings are load-bearing and not arbitrary:

- **E before the mangling change**, because the symbol encoding is a function of the target
  model.
- **D before A.** Removing the inherent fallback deletes `type_implements_spec`'s
  signature-matching search, which is currently the *only* thing making `spec *Animal`
  coercion work for a type composed with `Mammal`. Doing A first would break that with no
  replacement in place.

B and C come early despite being independent: they make the registry correct, so every later
step is tested against a registry that answers truthfully.

### Risks and open questions

- **S3 may not be safely reachable.** Inferring a method's `spec T` return type requires
  checking its body during the signature phase, while its own type's cell is mid-population
  — a body referencing `Self`'s fields would observe an incomplete cell. `spec_return_inference_stack`
  guards recursion for free functions but not this. It is sequenced last, alone, so it can be
  abandoned without touching anything else. **If it needs cell completion, stop and report
  rather than forcing it** — a partially-populated cell reaching user code is worse than the
  gap.
- **The closure walk (D) must terminate.** `resolve_spec_declaration` already guards spec
  cycles at declaration; the closure walk needs its own visited set, since a diamond
  (`C : A, B` where both `: Base`) would otherwise register `Base` twice and trip
  `DuplicateCompose`.
- **`lookup_key` is a completeness obligation, not a fix.** Any refinement later added to
  `ResolvedType` must be considered here. Say so in the function's own doc comment.
- **Symbol re-baselining.** After the mangling step, `nm target/core.o` changes by design.
  Capture before/after and confirm the *only* differences are targets that previously
  rendered through `Display`.
- **`for..in` grammar (S4)** touches the parser's `for`/`for..in` disambiguation, which is
  already delicate (both start with `for`). If the annotation creates a new ambiguity with
  the C-style form's init clause, flag it rather than adding lookahead depth.

## Implementation Plan

1. **Target model (E, part 1).** Introduce `ComposeTarget`; widen the `compose`/`primitive`
   lookahead to admit `[`; teach `match_compose_target` the `[?]T` shape; construct
   `ComposeTargetNotAType` for pointer/array/spec/function targets. Fixes #8, #11. Slice
   composes now work; blanket rejection must still fire.
2. **Canonical lookup key (B).** Add `ResolvedType::lookup_key()` and apply it at
   registration and lookup in all five sites. Fixes #4, #5, and the "miss" half of #6.
3. **Per-instantiation identity (C).** `method_ids` on both entry types via
   `identity_for`; `check_compose_block` stamps them. Fixes #2 and the panic half of #6.
4. **Structural target mangling (E, part 2).** Encode targets via `MangleType`; delete the
   `Display` fallback. Fixes #7. **Re-baseline `nm target/core.o`.**
5. **Dependency-closure registration (D).** Register derived entries over the closure with
   deferred arg resolution and a visited set; flag them; narrow `check_generic_bounds` to
   seed only the declared bound. Fixes #1. The acceptance test is
   `examples/dev/main.omg:514` — `value.call_something_else()` inside
   `accepts_myspec<T: MySpec>` (declared at `:513`), the one receiver call in the tree that
   reaches a compose through a spec *alias*.
6. **Remove both conformance fallbacks (A) + block-contents check (F).** Delete
   `check_compose_block`'s `inherent` parameter and `type_implements_spec`'s search; match
   the extra-function guard on `(name, signature)`; delete the now-unreachable
   `ItemKey`-keyed pending queue (I1). Migrate `core`'s ~25 spec-satisfying method bodies out
   of `primitive` blocks into their composes, and qualify `std::hash_map`'s five duck-typed
   calls. Fixes #3, #9, #10, I1.
7. **Spec coercion positions (S1)** — three sites.
8. **`is_variadic` on spec functions (S2).**
9. **`for..in` binding annotation (S4)**, plus the ambiguity error when unannotated.
10. **`spec T` return inference on methods (S3)** — alone, last, abandonable.
11. **Docs and comments.** The six stale `for`-attached comments (#12); reword I2 from a
    limitation to a stated rule; update `docs/08`, `05`, `13`, `18`, `24`, and move every
    fixed tracker entry to a "Fixed" note in its topic file rather than deleting the line.

## Testing

**New cases** (each is a currently-failing reproducer; all are recorded in
`docs/14-known-issues.md` with their exact source):
- `compose [?]u8 : Eq` and `compose<T> [?]T : Eq`, both used through a bound and a
  spec-qualified call.
- `abc : Shape::Circle` — field access, inherent method, *and* composed method all resolve.
- Match-narrowed binding reaching a composed method.
- `mut a: [4]u8; rw := &mut a[0..]; rw.is_empty()` — must compile, not panic.
- Both `&a[0..]` and `&mut a[0..]` of one element type in one program.
- `compose<T> Box<T> : S` instantiated at `Box<i32>` and `Box<u8>` in one program.
- `spec MySpec = Dummy | Mammal` with `compose Wolf : Mammal`, used through `T: MySpec`
  **and** coerced to `spec *Animal`.
- The three S1 positions: bare tail return, struct-literal field, array-literal element.
- A type composed with `ToIterator<u8>` and `ToIterator<char>`, iterated with
  `for x : u8 in ...`.

**Negative cases:**
- `42.fmt(w)` → `MethodNotInScope` naming `Display` and offering both fixes. **Assert the
  rendered text**, not just the variant — this is the plan's headline diagnostic and the
  existing test only matches the enum.
- `compose *Foo : S` → `ComposeTargetNotAType`, not a parse error about `':'`.
- A compose omitting a requirement the target happens to have inherently →
  `MissingSpecFunction` (this is the behaviour change in step 6; it must not silently pass).
- An extra overload of a required name → `ComposeExtraFunction` naming the signature.
- A `hidden` inherent method plus `compose Foo : ExposedSpec {}` → now
  `MissingSpecFunction`, since the fallback is gone.
- `compose<T: Numeric> T : Sum` → still `BlanketComposeNotYetSupported`.
- Unannotated `for x in y` where `y` composes `ToIterator` twice → ambiguity naming each `T`.

**Regression risk:**
- `just test-io` byte-identical at every step; `just run-exec` exit 69 at every step. These
  are the only end-to-end checks, and step 6 rewrites `core`'s primitive/compose split.
- `nm target/core.o` changes **only** at step 4, and only for targets that previously
  rendered through `Display`. Any other movement means the target model leaked into
  named-type mangling.
- `compiler/omega-mangle/tests/roundtrip.rs` should now *gain* cases (structural targets
  become encodable); it must not need weakening.
- `compiler/omega-driver/tests/compose.rs` — every existing case must still pass, and the
  six delivered cases should be audited against step 6's behaviour change.
- `examples/dev/main.omg:514` is the single most sensitive line in the tree for step 5 —
  the only receiver call reaching a compose through a spec alias.

**Target coverage:**
- *Hosted:* `just build-core`, `build-plat`, `build-std`, `test-io`, `run-exec`, `build-io-demo`.
- *No-allocator:* a package registering `core` but not `plat`/`std`, using a `primitive`
  method, a slice compose, and a compose on a local type — links with only `UnfilledGap`.
