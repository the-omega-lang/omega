# `compose` and `primitive`: Omega's composition model

## Task Description

- **What is being asked:** Replace how a type comes to satisfy a spec.

  ```omega
  # before
  struct Dog : Animal {
      exposed id: i32;
      exposed kind(*self) => AnimalKind { AnimalKind::Dog }
      exposed make_sound(*self) => *str { "woof woof" }
  }

  exposed spec SignedIntegerOps : Eq, Ord, Default, Hash, Display for i32 {
      abs(*self) => Self { ... }
      equals(*self, other: Self) => bool { *self == other }
  }

  # after
  struct Dog {
      exposed id: i32;
  }

  compose Dog : Animal {
      kind(*self) => AnimalKind { AnimalKind::Dog }
      make_sound(*self) => *str { "woof woof" }
  }

  exposed primitive i32 {
      exposed abs(*self) => Self { ... }
  }

  compose i32 : Eq {
      equals(*self, other: Self) => bool { *self == other }
  }
  ```

  Three changes, one mechanism: `compose` becomes the only way a type satisfies a spec,
  `primitive` becomes built-in types' declaration site, and `spec ... for` plus
  declaration-site `implements` clauses are deleted.

- **Purpose:** Omega has no way to give a type behaviour from outside the type's own
  declaration. That single missing capability is why `core` cannot shed responsibility it
  does not want: `Hash` must live in `core` because only `core` may write `spec ... for i32`,
  and no package can ever attach anything to another package's types. `compose` supplies the
  capability; `primitive` gives built-ins an honest declaration site instead of borrowing
  `spec`'s grammar.

  Serves *modern syntax with real abstraction power* (one construct per concept), and
  *no hidden behavior*: after this change, a receiver call `x.m()` resolves against `x`'s own
  declaration or a spec you explicitly bounded on — never against something a third package
  attached out of sight.

- **Reasoning:**

  **Why `spec ... for` has to go.** It conflates two unrelated operations: giving `i32` an
  `abs` (inherent behaviour, where the spec name `SignedIntegerOps` is pure ceremony nobody
  writes, imports, or bounds on) and declaring that `i32` implements `Display` (conformance).
  Eight accommodations exist only to hold that conflation together — confinement to `core`
  for a *discovery* reason rather than a coherence one (`extensions.rs:22`), exactly one
  block per target globally, a bespoke tree walk because nothing can import a `for`-spec by
  name, spec functions having no visibility of their own, `for [?]T` targets accepting no
  visibility modifier at all (`slices.omg:17`), no per-function generic bounds (which is why
  slices have no `contains`), and the weak-linkage "compiled into the using side's TU" model.
  Splitting the two operations dissolves all eight.

  **Why composed methods live in the spec's namespace, not the type's.** This is the load-
  bearing decision and it is not aesthetic. Today `resolve_implements_clause` *merges*
  spec-satisfying methods into the implementor's own `functions` list, which is why
  `animal.make_sound()` works inside `make_sound_with_static_dispatch<T: Animal>`. Keeping
  that merge under an open orphan rule is incoherent: package A may write
  `compose Foo : SpecX` (type local) and package B `compose Foo : SpecY` (spec local), both
  legal, neither at fault, and if both supply a `fmt(*self, *mut Writer) => void` the
  collision only exists once the two are linked together. No orphan rule that permits both
  directions can prevent it. Rust answers with `<Foo as SpecX>::fmt`; that disambiguation
  syntax was explicitly rejected for Omega.

  Resolving composed methods through the *spec* makes the collision structurally impossible
  rather than diagnosable. `Foo`'s own method set is exactly what `struct Foo { }` declares,
  in every compilation, regardless of what any other package links in — which is the
  strongest form of "no hidden behavior" available here.

  **What that costs, and how it is paid.** `animal.make_sound()` above stops working for
  free, because `make_sound` is no longer `Dog`'s. It is recovered by the **bound context**:
  the `(concrete type, spec)` pairs the enclosing item's own generic bounds asserted, which
  `Driver::check_generic_bounds` already computes and currently discards. A receiver call
  resolves against the receiver's inherent methods, plus composed methods whose spec is in
  that context. `x.fmt(w)` under `T: Display` works; a bare `42.fmt(w)` does not, and becomes
  `Display::fmt(42, w)`. This generalizes the `ResolvedType::SpecObject` arm
  (`calls.rs:168`) that already does exactly this for `spec *T` — one rule, two dispatch
  strategies.

  **Why `primitive` blocks stop being special.** A `primitive` block is a declaration site
  with ordinary function bodies, so it is compiled by its owning package like any other item.
  That deletes `extensions.rs` outright — the lazy `core`-tree walk, the per-receiver caching,
  `drain_pending_extensions`, `CheckedFunctionDef::extension_target`'s weak-linkage model, and
  `compile.rs`'s `error_scope` extension. Nothing replaces it: the eager sweeps that already
  exist (`collect_signatures` locally, `collect_extern_signatures` for every `--extern`) see
  `primitive` and `compose` items by adding two variants to one `matches!`.

  Alternatives considered:

  - *Merge composed methods into the type's method list (today's model, new source).* Rejected
    on the coherence argument above. It is the smaller diff and it preserves every current
    call site, which is exactly what makes it tempting; it also makes a two-package collision
    unfixable by either package.
  - *Rust's "trait must be in scope" rule.* Rejected: it makes what a method call means depend
    on the enclosing module's imports, so adding an import silently changes behaviour.
  - *UFCS / extension methods (C#, Kotlin, Nim).* Rejected: static dispatch with no conformance
    means specs and extensions become two unrelated mechanisms for one concept.
  - *Keep `spec ... for` for primitives and add `compose` only for conformance.* Rejected: it
    keeps every one of the eight accommodations alive for the primitive half.

- **Resolved concerns:**

  - **The orphan rule as originally stated is unimplementable.** "Only specs declared in the
    current module" would mean no user type could ever implement `Display` — the spec's own
    package would have to compose it for every downstream type. Implemented as the
    disjunction: **a `compose` is legal iff the target type *or* the spec is local to the
    composing package**, at package granularity (same root module segment as
    `Visibility::Internal`). `core` owns every primitive by virtue of declaring it in a
    `primitive` block, so `compose i32 : Display` is legal in `core` *and* in whatever package
    declares `Display`.
  - **Blanket composes and specialization are out of scope**, by decision. `compose<T: Numeric>
    T : Sum` parses and produces a dedicated "not yet supported" diagnostic naming the
    follow-up. Nothing in `core`, `std`, or the examples needs one: `numerics` macro-generates
    twelve concrete blocks, and `for..in` gets its `Iterator`/`ToIterator` blanket behaviour by
    trying both specs (`classify_for_in_source`), not by a blanket impl. `compose` still takes
    a generic *list* — it is needed for a generic *target* (`compose<T> List<T> : ToIterator<T>`),
    which is not a blanket and is fully in scope.
  - **Nothing moves between `core` and `std`**, by decision. This plan changes the mechanism
    only; every module stays in the package it is in now. The orphan rule forces no move —
    `compose i32 : Display` is legal inside `core` today, since `core` owns both sides — so the
    `core`/`std` responsibility split is a separate question, planned separately, on top of a
    `compose` that already works.
  - **Compose methods carry no visibility modifier**, rejected at parse time. The stated rule
    was "inherited from the spec, and an individual method may not be more visible." Under the
    spec-namespace rule a composed method is only ever reachable *through* its spec, so a
    narrower method visibility would mean "you can see that `i32: Display` but may not call
    `fmt`" — incoherent for a conformance. Uniform inheritance is what rule (d)'s own first
    sentence says, and it deletes `SpecMethodTooHidden` and the `own.visibility < req.visibility`
    check entirely rather than inverting them. Spec functions already have no visibility of
    their own (`FlattenedSpecFn::visibility`), so this makes the two consistent.
  - **`primitive` carries no block-level visibility either**, which departs from the example as
    written (`exposed primitive i32 { abs(self) => ... }`). A primitive's *type* is built in and
    always visible, so a block-level modifier has nothing to control; its functions carry their
    own, exactly like a struct's members. This is what removes the `for [?]T` parser limitation
    (`slices.omg:17`) rather than carrying it forward. The migration writes `exposed` per
    function; `numerics` is macro-generated, so the verbosity costs nothing there.
  - **Spec-qualified calls adapt their receiver.** `Display::fmt(x, out)` adapts `x` to `fmt`'s
    declared self-mode using `adapt_self_argument`, exactly as `x.fmt(out)` would. Without this
    the print macros could not work at all — `($args).fmt(...)` becomes
    `Display::fmt($args, ...)`, and a macro cannot know whether its argument needs a `&`
    (`&(*str)` would be a pointer to a pointer). The original `compose` example already assumed
    this: it writes `Printable::print(item)`, not `Printable::print(&item)`.
  - **Static composed functions are reached through the type, not the spec.**
    `Default::default()` has no receiver to infer `Self` from, so a self-less composed function
    is called as `i32::default()`. `resolve_type_member` searches inherent members first, then
    every compose on that type, and reports `AmbiguousComposedStatic` naming both specs if two
    provide the same name. This is deliberately asymmetric with instance calls, and the reason
    is precise: a static call writes its type at the call site, so a collision is local and
    fixable; an instance call does not, which is where the two-package incoherence actually
    bites. `Default::default` is the only self-less spec function in the whole tree.
  - **Spec-to-spec inheritance survives untouched.** `spec Mammal : Animal, Dummy` and
    `spec MySpec = Dummy | Mammal` are not type-to-spec conformance; `parse_optional_implements`
    is shared between the two positions today, and only the type-declaration callers lose it.
  - **A spec default body is the spec's code, not the implementor's.**
    `check_pending_spec_method` currently sets `current_owner` to the implementor
    (`items.rs:1235`), granting a default body the same hidden-field access as a hand-written
    method. That must be removed: under `compose`, `Self` may be a type in another package
    entirely. Verified safe against the current tree — every spec default body in `core`,
    `std`, and the examples calls methods on `self`, never reads a field.

## Technical Details

### The three forms

```
compose <Target> : <Spec> { <function definitions — no visibility modifiers> }
compose<T, U> <Target-using-T> : <Spec> { ... }

exposed? primitive <PrimitiveType> { <function definitions — own modifiers> }
primitive<T> [?]T { ... }
```

`compose`'s generic list binds parameters used in the *target* (`compose<T> List<T> :
ToIterator<T>`). A generic parameter that appears only in a bound and not in the target is a
blanket compose, rejected for now with a diagnostic naming the follow-up plan.

Both are **contextual keywords recognized only at item position**, exactly like
`gap`/`glue`/`marker`/`exposed`. One-token lookahead separates a declaration from a top-level
binding: after `compose`/`primitive`, `:` or `:=` means a binding, an identifier or `<` means a
declaration. `i32` is a plain `Ident` to the lexer, not a keyword, so `primitive i32 {` needs no
special handling.

### The compose registry

One table replaces both `ResolvedStructType::implemented_specs` (and its two siblings) and
`Extensions::resolved`:

```rust
struct ComposeEntry {
    module: ModulePath,          // where it was declared: codegen placement + diagnostics
    id: HirId, span: Span,
    target: ResolvedType,
    spec: Rc<RefCell<ResolvedSpecType>>,
    spec_args: Vec<ResolvedType>,
    /// One per flattened requirement, in `flatten_spec` order — which is
    /// also the vtable slot order (see `CheckedSpecCoerce::slots`).
    methods: Vec<(Ident, ResolvedMethod)>,
    pending: Vec<PendingSpecMethod>,
}
```

Keyed on `(spec id, spec_args, target)`. A `ResolvedType` key is sound for the reason
`Extensions::resolved` already documents (`extensions.rs:65-68`): every cell it can contain
hashes and compares by an `id` decided once at creation.

Populated by `collect_compose_signatures(&compose_modules)`, modelled directly on
`collect_glue_signatures` (`compile.rs:247`) including its deduplication of
`extern_surface ++ local` — a package registered as its own `--extern` must not have every
compose reported as a duplicate of itself.

### The bound context

`Analyzer` gains `bounds: Vec<(ResolvedType, Rc<RefCell<ResolvedSpecType>>, Vec<ResolvedType>)>`.
Seeded from three places, all of which already have the information:

1. `Driver::check_generic_bounds` — `(param.bound, concrete)` pairs, currently checked and
   discarded.
2. A spec default body — the declaring spec, with `Self` as the target.
3. A compose block body — the composed spec and its target.

To avoid touching all nineteen `analyze`/`with_analyzer` call sites, add one entry point
(`with_analyzer_in(module, substitution, bounds, owner, f)`) and have the existing
`with_analyzer` delegate to it with empty bounds.

### Method resolution after this change

| Call shape | Resolves against |
|---|---|
| `x.m(...)`, `x` concrete | `x`'s inherent methods, plus composed methods whose spec is in the bound context |
| `x.m(...)`, `x: spec *S` | `S`'s flattened list, dynamically (unchanged — `calls.rs:168`) |
| `Spec::m(x, ...)` | the compose for `(Spec, typeof x)`; `x` adapted to `m`'s self-mode |
| `Type::f(...)` | `Type`'s inherent statics, then composes on `Type`; ambiguity is an error |

### What changes

**Parser** (`omega-parser`)
- `parse_optional_implements` (`item.rs:529`) keeps its *spec dependency* caller and loses its
  three type-declaration callers: `parse_struct_or_marker_body` (`:623`), `parse_union_def`
  (`:691`), `parse_enum_def` (`:894`). Rename to `parse_spec_dependencies` once it has one
  caller.
- `parse_spec_def` (`:736`) drops the `for` clause (`:769`). `TokenKind::For` stays — it is the
  `for` loop's token.
- New `parse_compose_def` and `parse_primitive_def`, plus their `Item`/`HirItem` variants.
  Reject a visibility modifier on a compose function; reject a block-level modifier on
  `primitive`.
- Delete `StructStmt::implements`, `UnionStmt::implements`, `EnumStmt::implements`,
  `SpecStmt::target`.

**HIR** (`omega-hir/src/hir.rs`)
- New `HirComposeDef { id, span, generics, target: Type, spec: Type, functions: Vec<HirFunctionDef> }`
  and `HirPrimitiveDef { id, span, generics, target: Type, functions: Vec<HirFunctionDef> }`.
- Delete `implements` from `HirStructDef`/`HirUnionDef`/`HirEnumDef` and `target` from
  `HirSpecDef`.

**Analyzer** (`omega-analyzer`)
- `resolve_implements_clause` (`specs.rs:818`) becomes `check_compose_block`: same
  `flatten_spec` machinery, same "own method or queued default" split, but keyed on one spec
  rather than a list, and it *returns* the method list instead of merging it into the
  implementor. Delete the cross-entry merge loop (`:829-855`) — one compose names one spec, so
  there is nothing to merge across. Delete the `SpecMethodTooHidden` check (`:866-878`).
- `collect_methods` (`items.rs:598`) loses its `implements`/`resolve_implements_clause` tail;
  a type's `functions` list is exactly its declared methods. `SpecMethods`'s third element
  (`implemented_specs`) disappears with it.
- `ResolvedMethod` gains `source: Option<ComposeSource>` so a method knows which spec, if any,
  it came through. Inherent methods are `None`.
- `find_methods` (`places.rs:324`) — the `other` arm's `resolver.extension_methods` call
  becomes a primitive-table lookup; every arm gains the bound-context pass.
- `resolve_type_member` (`paths.rs:442`) — same, plus the composed-static fallback.
- `type_implements_spec` (`specs.rs:1004`) becomes a registry lookup returning the entry's
  `methods`' `decl_id`s as vtable slots — it stops re-deriving conformance by searching method
  lists for matching signatures.
- `for_in_source_declares` (`specs.rs:606`) becomes a registry lookup, and stops being
  restricted to struct/enum/union: a composed primitive can now be iterable.
- Delete `ExtensionTarget`, `resolve_extension_target` (`:313`), `resolve_extension_methods`
  (`:369`), `is_slice_extension_target` (`:195`), `is_extendable_primitive` (`:282`).
- `check_pending_spec_method` (`items.rs:1222`) — delete the `current_owner` assignment.
- `Context::resolve_pointer_type`'s `Self` re-stamping (`context.rs:605`) gains a
  `ResolvedType::Slice` arm, so `primitive<T> [?]T` can bind `Self` to `[?]T` directly instead
  of to the `Array` shape the current workaround needs.
- `error/kind.rs` — delete `ExtensionOutsideCore`, `DuplicateExtensionTarget`,
  `ExtensionTargetNotAllowed`, `ExtensionSelfMustBePointer`, `SpecMethodTooHidden`. Add
  `ComposeOrphanViolation`, `ComposeTargetNotAType`, `DuplicateCompose`, `ComposeExtraFunction`,
  `BlanketComposeNotYetSupported`, `PrimitiveOutsideCore`, `PrimitiveTargetNotAllowed`,
  `DuplicatePrimitiveTarget`, `AmbiguousComposedStatic`, and a `MethodNotInScope` that names the
  spec and suggests the bound or the qualified form.

**Driver** (`omega-driver`)
- **Delete `extensions.rs`** and its five call sites: `methods_attached_to`,
  `drain_pending_extensions` (`compile.rs:98`), `reject_local_extensions` (`:461`), the
  `error_scope` extension (`:105`), and `ModuleResolver::extension_methods`
  (`resolver.rs:427`, `omega-analyzer/src/resolver.rs:470`). `CORE_MODULE`/`is_core_module`
  move to a small module of their own — `roots.rs:24` and `ambient_core_candidates` still need
  them.
- New `composes.rs`: the registry, the orphan-rule check, `collect_compose_signatures`, and
  the primitive method table.
- `collect_extern_signatures` (`compile.rs:236`) — add `HirItem::Primitive`/`HirItem::Compose`
  to the `matches!`.
- `compute_item` — new arms for both. A `primitive` block's target is not an item name, so it
  gets no `ItemKey`; like `Glue`, it is swept rather than name-resolved.
- `check_module_bodies` — compose and primitive bodies are checked here, in their own module,
  like any other item.

**Codegen** (`omega-codegen`)
- `CheckedFunctionDef::extension_target: Option<ResolvedType>` (`checked.rs:197`,
  `mir.rs:76`, `item.rs:65-74`) becomes `compose_owner: Option<ComposeOwner>` carrying target,
  spec module, spec name, and spec args — a like-for-like swap on a field that already threads
  the whole pipeline.
- New `mangle::compose_method_symbol`, built from `vtable_symbol`'s existing nesting
  (`mangle.rs:184`): `<target>::<Spec>[<args>]::<method>` in the value namespace. No new
  `omega_mangle` production; `roundtrip.rs` should not need to change.
- A `primitive` block's method is an ordinary method symbol on its target type.

**Runtime and examples** — 15 declaration-site `implements` clauses (`std::list`,
`linked_list`, `hash_map`, `hash_set`; `examples/dev` ×6; `examples/io_demo` ×1) become
`compose` blocks. `core`'s seven `for` blocks across five files (`numerics` ×3, macro-generated
over twelve scalar types; `strings`, `chars`, `bools`, `slices` ×1 each) split into a
`primitive` block plus one `compose` per satisfied spec. Concrete call sites that reach a composed method on a concrete receiver are rewritten
to the qualified form: `core::io`'s four print macros (`($args).fmt(...)` →
`Display::fmt($args, ...)`), `std::io:58`, `examples/io_demo:18`, and
`examples/dev:1178`'s `score.describe()`.

**Docs** — `08-specs.md` (conformance, `for`-attached specs, method resolution),
`13-core-library.md`, `20-marker-types.md` (markers lose their `implements` clause),
`23-standard-library.md`, `24-console-io.md` (its "Caveats" section describes exactly the two
things that change), `18-for-in-loops.md`, `06-generics.md` (bounds now govern method
visibility inside a generic body), `07-visibility.md` (compose blocks and `current_owner`),
`10-modules-and-linkage.md` (the weak-linkage extension model is gone).

### What must not change

- **Spec-to-spec dependencies and spec aliases.** `spec Mammal : Animal, Dummy`,
  `spec MySpec = Dummy | Mammal`, and dependency flattening order are untouched.
- **`flatten_spec`'s ordering and dedup rules.** It stays the single source of vtable slot
  order.
- **`spec *T` dynamic dispatch.** `finish_dynamic_dispatch_call` and the vtable data layout are
  unchanged; only where the slot list is *sourced* from moves.
- **`gap`/`glue`.** Entirely separate mechanism, first-classed in the previous plan. No glued
  symbol may move.
- **`marker`** as a type kind, including `Unit`'s role in `HashSet<T>`. It only loses its
  `implements` clause.
- **`Option`'s variant order** (`None = 0`, `Some = 1`), load-bearing in `analyze_for_in`.
- **Package layout.** No module moves between `core` and `std`.
- **`resolve_overload`.** Overload resolution among a type's own methods is unchanged; no
  specificity ranking is added.

### Chosen approach

Six stages, each leaving the tree buildable and each verifiable against the existing
end-to-end tests (`just test-io` diffs real output; `just run-exec` must keep exiting 69).

The pivot is stage 1: **`struct Foo : Spec { }` is desugared into an implicit compose** the
moment `compose` exists. From that point there is exactly one conformance model, so stages 2
and 4 are purely syntactic migrations of source that already behaves identically. The
alternative — two conformance models coexisting until the end — would mean every intermediate
stage tests a configuration that never ships.

Stage 1 is where the semantic break lands, deliberately and all at once: a spec-default method
stops being reachable on a concrete receiver. Under the desugar an *overriding* method is still
inherent (it is declared in the type's body), so the blast radius is exactly the default-body
call sites listed above — four macros and three source lines.

### Risks and open questions

- **The bound context may not reach every case.** The three seeding points cover every
  construct in the tree today. If a fourth appears — most likely a generic *method* on a
  generic type, whose own parameter bounds are collected by `collect_function_signature` rather
  than `check_generic_bounds` — flag it rather than widening `find_methods` to search all
  composes, which would reintroduce the incoherence this design exists to prevent.
- **Compose discovery cost.** Every compose in every registered `--extern` is signature-resolved
  eagerly, roughly 60 in `core` after migration. This is strictly less work than today's
  `discover_extensions`, which eagerly resolves every `for` block in `core`'s tree plus a
  `flatten_spec` per target. If it measurably regresses build time, say so; do not add lazy
  discovery without a plan for it.
- **Cross-package cell mutation is deliberately avoided.** The registry is separate from the
  type cells, so composing a foreign type never mutates that package's `ResolvedStructType`. If
  an implementation shortcut tempts you to patch the cell instead, don't — it would make a
  cell's contents depend on which composes the current compilation happens to see.
- **`Self` inside `primitive<T> [?]T`.** The `Slice` arm added to `resolve_pointer_type` is the
  one place this could go wrong; `SliceImpl<T>::first(*self, out: *mut T)` called with
  `T = *str` is the exact case `context.rs:597-604` documents as a real, previously-hit bug.
  Test it explicitly.
- **Diagnostic quality for the most common new mistake.** `42.fmt(w)` will be written by
  everyone at least once. `MethodNotInScope` must name the spec, say that `fmt` comes from
  `Display`, and offer both fixes (`Display::fmt(42, w)`, or a `T: Display` bound). A bare "no
  method named `fmt`" would be a serious regression in usability.

## Implementation Plan

1. **Add `compose` end to end, with declaration-site conformance desugared onto it.**
   Parser (`parse_compose_def`, contextual keyword, blanket rejection), `HirComposeDef`, the
   registry and orphan rule in a new `omega-driver/src/composes.rs`,
   `collect_compose_signatures` modelled on `collect_glue_signatures`, `check_compose_block`
   out of `resolve_implements_clause`, the bound context and its three seeding points, the four
   resolution paths in the table above, `type_implements_spec`/`for_in_source_declares` moved
   onto the registry, `compose_method_symbol`, and `compose_owner` replacing
   `extension_target`. `struct Foo : Spec { }` still parses and is desugared to a compose whose
   methods are `Foo`'s own matching declarations.

   Fix the call sites the semantic change breaks: `core::io`'s four print macros,
   `std/io.omg:58`, `examples/io_demo/main.omg:18`, `examples/dev/main.omg:1178`.

   Build core, std, plat, io_demo; `just test-io` must diff clean and `just run-exec` must exit
   69. This is the largest step and the only one that changes behaviour.

2. **Migrate declaration-site conformance to explicit `compose` blocks.** All 15 sites. Purely
   syntactic given step 1's desugar; same test gate. Nothing in the compiler changes.

3. **Add `primitive`.** Parser, `HirPrimitiveDef`, the `core`-only check, one-block-per-target,
   the primitive method table, the `Slice` arm in `resolve_pointer_type`, and body checking in
   the declaring module. `spec ... for` still works; both forms coexist for one step.

4. **Migrate `core`'s `for` blocks.** `numerics` (three macros), `strings`, `chars`, `bools`,
   `slices` split into `primitive` plus one `compose` per satisfied spec. Same test gate, and
   `nm target/core.o` should differ only in the symbols whose owning construct changed — a
   primitive method's symbol is unchanged, a composed method's moves from the old
   extension-target mangling to `<target>::<Spec>::<method>`.

5. **Delete the old path.** `spec ... for` grammar and `HirSpecDef::target`; `implements` on all
   three type declarations and their AST/HIR fields; the desugar from step 1;
   `omega-driver/src/extensions.rs` and its five call sites; `ModuleResolver::extension_methods`;
   `implemented_specs` on all three cells; `SpecMethods`'s third element; and the five listed
   diagnostics. Move `CORE_MODULE`/`is_core_module` somewhere that isn't a deleted file.

6. **Docs.** The ten files listed above. `08-specs.md` is a genuine rewrite, not a
   find-and-replace: its "for-attached specs" section and its account of how a method call
   resolves both describe a model that no longer exists.

## Testing

**New cases:**
- A local type composed with a foreign spec (`compose Pair : Display` in `io_demo`), and a
  local spec composed onto a foreign type — the two halves of the orphan rule, each in a
  separate package.
- A generic target: `compose<T> List<T> : ToIterator<T>`, instantiated at two different `T`.
- The same generic spec composed twice at different arguments on one type
  (`ToIterator<char>` and `ToIterator<*char>` — the case `resolve_implements_clause`'s merge
  loop exists for today).
- Bound-context resolution: `f<T: Animal>(a: *T) { a.make_sound(); }` at two concrete `T`.
- `spec *Animal` coercion and dynamic call, with the concrete methods supplied by a compose —
  the vtable slot order must match `flatten_spec`'s.
- `Display::fmt(42, w)` and `Display::fmt("hi", w)` — the receiver-adaptation case, covering
  both a by-value primitive and a `*str` whose `*self` re-stamps.
- `i32::default()` — a self-less composed function reached through its type.
- `for x in collection` where the `ToIterator` conformance comes from a compose in a different
  package from the collection.
- A spec default body (`Ord::less_than`) instantiated for a composed type, calling `self.compare()`
  through the spec it is declared in.
- `primitive<T> [?]T`'s `first(*self, out: *mut T)` at `T = *str`.

**Negative cases:**
- `compose ForeignType : ForeignSpec` → `ComposeOrphanViolation`, naming both packages and
  stating that one of the two must be local.
- Two composes for the same `(type, spec, args)` → `DuplicateCompose`, naming both sites.
- A compose missing a required function, or declaring one the spec does not have → names the
  function and the spec.
- `exposed equals(*self, ...)` inside a compose → rejected at parse, stating that a composed
  method inherits its spec's visibility.
- `compose<T: Numeric> T : Sum` → `BlanketComposeNotYetSupported`, naming the follow-up.
- `42.fmt(w)` → `MethodNotInScope` naming `Display` and offering both fixes. This diagnostic's
  text is part of the feature; assert on it.
- `primitive i32 { }` outside `core` → `PrimitiveOutsideCore`.
- `primitive MyStruct { }` → `PrimitiveTargetNotAllowed`.
- Two `primitive` blocks for one target, in different `core` modules → `DuplicatePrimitiveTarget`
  naming both.
- `struct Foo : Bar { }` after step 5 → a parse error saying conformance is declared with
  `compose`, not at the declaration site.
- A compose body reading a `hidden` field of its target without `reveal` → `FieldNotVisible`.
  The same body with `reveal` → accepted. This is rule (e), and `current_owner` is the only
  thing enforcing it.

**Regression risk:**
- `tests/io_demo.expected` byte-identical at every step. It is the only test that checks real
  program output, and the print macros change in step 1.
- `just run-exec` exit code 69 at every step — `examples/dev/main.omg` is ~1400 lines
  exercising specs, generics, dynamic dispatch, `for..in`, and overloads together.
- `compiler/omega-mangle/tests/roundtrip.rs` should be untouched. If it needs changing, the
  compose symbol scheme drifted from `vtable_symbol`'s existing shape.
- `check_generic_bounds`'s existing `SpecNotImplemented` path — its `missing` list becomes
  vestigial once conformance is a registry lookup. Simplify the diagnostic rather than passing
  an always-empty vector.
- Grep for `for`-block and `implements` references across `docs/` at the end; a stale one is a
  silent documentation bug, not a build failure.

**Target coverage:**
- *Hosted:* `just build-core`, `build-plat`, `build-std`, `test-io`, `run-exec`.
- *No-allocator:* a package registering `core` but not `plat` or `std`, using a `primitive`
  method and a `compose` on a local type — must compile and link with only an `UnfilledGap`
  warning. This is the check that `compose` and `primitive` introduce no allocation, no runtime
  support, and no gap dependency.
