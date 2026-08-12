# Known issues tracker

A single consolidated list of every confirmed, currently-unfixed gap
described in these docs, for tracking at a glance. Each entry links to its
full writeup. Update this file whenever a gap here is fixed (move it to a
"Fixed" note in the relevant topic file, don't just delete the line) or a
new one is found.

## Codegen

- **A float argument to a variadic (`printf`-style) call reads garbage
  from whatever's left in `%al`.** Not this compiler's bug — Cranelift
  itself has no support for the x86-64 SysV vararg calling convention
  (`%al` must hold the caller's XMM-register count; confirmed nothing in
  `cranelift-codegen` handles this at all, and `rustc_codegen_cranelift`
  hit the same wall and just forbids the shape). Surfaces differently
  depending on register allocation: a function parameter forwarded
  straight into a variadic call (any `-O` level), an enum body-field
  projection (`-O1`+ only), or even a plain local in a large enough
  function (previously believed safe; it isn't, it was just small
  enough to not show it). [primitives.md](01-primitives.md)
- **No real C-ABI aggregate-passing convention** — structs/enums pass as
  flattened positional scalars, fine Omega-to-Omega, not safely callable
  from hand-written C expecting real struct-passing rules.
  [primitives.md](01-primitives.md)
- **Extern *data* declarations (a non-function `extern`) have no storage
  story** — fully resolved and type-checked like everything else, but
  codegen still has nothing sound to do with it (`todo!()` in
  `update_extern_decl`), since its storage genuinely lives in another
  translation unit. An ordinary top-level global (`ident: type;`, or a
  non-`comp` `ident := comp value;`) is *not* this gap anymore — both
  are fully implemented, including `mut`. [mir-and-codegen.md](16-mir-and-codegen.md),
  [compile-time-evaluation.md](19-compile-time-evaluation.md)
- **Assigning *into* a function parameter directly (no deref in between)
  is still `todo!()`** — taking a parameter's *address* is fixed (see
  [mir-and-codegen.md](16-mir-and-codegen.md)'s own "Fixed" note); direct
  assignment is a separate, still-unfixed code path. An explicit local
  copy works around it today. [mir-and-codegen.md](16-mir-and-codegen.md)

## Types

- **`*str` is not actually guaranteed valid UTF-8** — casting between
  `*str` and `*[?]u8`/`*[?]i8` is unsound in both directions, no validation.
  Deliberately deferred pending a `core`-provided validating conversion.
  [strings-casting-and-slices.md](11-strings-casting-and-slices.md)
- **`char`/pointer arithmetic and `bool` logical-not are fixed; casting an
  arbitrary integer into `char`/`bool` and the `!` operator are still not
  implemented.** `char`/pointers now get arithmetic/bitwise ops (and
  casting out to any numeric type) by coercing to `u32`/`usize` first,
  never back implicitly; `bool` now gets native `== != & | ^`. What's
  still missing: casting an arbitrary integer *into* `char` (only `u8` is
  guaranteed to produce a valid codepoint, so only that direction is
  allowed — a real validating path, e.g. a fallible `char::from_u32`-style
  constructor, is deliberate future work, not solved narrowly here), and
  a `!` (logical-not) operator for `bool` (a real, if small, language
  feature — a new parser token plus a new `Expression`/`HirExpr`/
  `CheckedExpr`/`MirExpr` variant — left for a dedicated follow-up rather
  than folded into an otherwise analyzer-only change).
  [primitives.md](01-primitives.md), [control-flow.md](03-control-flow.md)
- **`core::fmt`'s float output is fixed-precision, not round-trip** — six
  fractional digits, with a scientific fallback below `1e-6` and at or above
  `1e19` whose normalization loop (repeated multiply/divide by ten) is itself
  lossy. `nan`/`inf`/`-inf` are exact. A shortest-round-trip formatter
  (Ryu/Grisu-class) is deliberate future work, not a narrow fix here.
  [console-io.md](24-console-io.md)

## Composition (`compose` / `primitive`)

### Found by review of the composition/spec structural repair

These are new, and were introduced (or left half-done) by the dependency-closure
and target-model work. None is a safe local patch; each needs a design decision.

- **A spec *alias* bound is no longer satisfiable through its members'
  composes.** `spec MySpec = Dummy | Mammal` with `compose Wolf : Mammal`
  (where `Mammal : Animal, Dummy`) now fails with `'Wolf' does not implement
  spec 'MySpec'`, listing every requirement as missing.
  `examples/dev/main.omg:1183` reproduces it, and `just run-exec` cannot
  build because of it.

  Mechanism: dependency-closure registration
  (`Driver::register_derived_composes` →
  `Analyzer::compose_dependency_closure`) walks *downward* — `compose T : S`
  registers derived entries for everything in `S`'s `dependencies` closure.
  An alias needs the *upward* direction: `MySpec` has no compose entry of its
  own, and `type_implements_spec`'s `Ok(None)` arm now returns "all
  requirements missing" instead of the signature search that used to cover
  this. Resolving it means deciding whether an alias (or any dependency-only
  spec) can have its conformance *synthesized* from its members' entries at
  lookup time, and where that synthesis lives — a registry-model decision,
  not a local fix. [specs.md](08-specs.md)

- **A derived entry silently shadows a later directly-declared compose for
  the same spec.** `compose Foo : Derived` (where `Derived : Base`) followed
  by `compose Foo : Base { b(*self) => i32 { 99 } }` reports no
  `DuplicateCompose`: `Driver::reject_duplicate_compose` skips derived
  entries, so the direct `Base` compose registers as a *second* entry for
  `(Foo, Base)`. `compose_for` takes the first match, which is the derived
  one, so `Base::b(&foo)` calls the `Derived` block's body while the
  explicitly written one is compiled, emitted, and never reached. Either the
  second declaration must be `DuplicateCompose` (a type conforming twice) or
  a direct entry must displace a derived one; both are semantic decisions.

- **`compose_dependency_closure` matches methods by exact
  `ResolvedFunctionType` equality**, not `fn_satisfies_requirement`, so a
  derived entry can be registered with *fewer* methods than the dependency
  requires. `spec Seq<T> : ToIterator<T>` composed with a concrete
  `to_iterator(*self) => It` (the normal spelling — `ToIterator`'s
  requirement carries a `spec Iterator<T>` return *bound*, never an exact
  type) registers a `ToIterator<u8>` entry with an empty method list. The
  visible symptom is a nonsense diagnostic (`method 'to_iterator' comes from
  spec 'Seq' but is not in this bound context`); the latent one is
  `type_implements_spec` returning a slot list shorter than the spec's
  requirements, since it maps the entry's `methods` straight to vtable slots
  with no arity check against `flatten_spec`. Not fixed here because the
  closure needs the requirement's `return_type_bound` — the same input
  `check_compose_block` uses — and threading it through changes what a
  derived entry *is*.

- **Slice compose targets parse, register, and mangle, but no call can ever
  reach one.** `compose [?]u8 : Eq { equals(*self, ...) }` compiles, yet a
  spec-qualified call reports `expected '**[?]u8' ... found '*[?]u8'` and a
  bound-driven call (`f<T: Eq>(x: T)`) aborts in the cranelift verifier
  (`mismatched argument count for call_indirect`).

  Mechanism: `instantiate_primitive` re-shapes `Self` for a `[?]T` target
  (`ResolvedType::Slice` → `ResolvedType::Array`) precisely so
  `Context::resolve_pointer_type` collapses `*self` to the fat pointer;
  `instantiate_compose` binds `Self` to the `Slice` directly, so `*self`
  becomes `Pointer{Slice}`. Copying the primitive's re-shape is *not*
  sufficient — it was tried and moves the failure to
  `fn_satisfies_requirement`, because the requirement's own `*self` is built
  by `flatten_spec` from the same `Slice`-shaped target. The two sides
  disagree about what `Self` means for a structural target, and picking one
  answer is a model decision that spans `flatten_spec`,
  `resolve_compose_target`, and `resolve_pointer_type`. Until then, slice
  conformance is declarable but unusable. [specs.md](08-specs.md)

- **`compose<T> *T : Spec` is still silently dropped.** The non-generic
  spelling (`compose *Foo : Spec`) now correctly reports
  `ComposeTargetNotAType`, but the generic one passes `blanket_parameter`
  (`T` does occur in `*T`), registers as a template, and is never
  instantiated — so `resolve_compose_target`, which owns the diagnostic, is
  never reached. The program compiles with no diagnostic at all. The fix is
  to validate a template's target shape at `collect_compose_signatures`
  time, which needs target validation split from target *resolution*.

- **Primitive-method symbols still embed `ResolvedType`'s `Display`
  output.** Structural-target mangling was applied to `compose_method_symbol`
  only; `ExternFunctionKind::Primitive`
  (`omega-codegen/src/cranelift/function.rs:280`) and its declare-pass twin
  (`cranelift/item.rs:107`) still build `Ident(target.to_string())`. So
  `nm target/core.o` still contains
  `_omg_NvNtNtC4core7strings4*str8is_emptyTEb`, and a local
  `primitive<T> [?]T` method still emits `_omg_NvNtC4main6*[?]u88is_empty…`
  — symbols outside `[A-Za-z0-9_]` that `omg_demangle` cannot round-trip,
  which is the original complaint. (The *space* in `*mut [?]u8` is gone, but
  only as a side effect of `lookup_key` normalizing mutability.) Not fixed
  here because `ManglePath::Type` is a root-shaped node while a primitive
  method's symbol nests its owner under a module path; giving structural
  primitive owners a spelling is a grammar decision, and it moves ABI.

- **`AmbiguousForLoopElementType` fires with an empty candidate list when a
  `for x : T in` annotation matches nothing.** `classify_for_in_source`
  filters the `ToIterator` entries by the annotation and then reports
  ambiguity whenever the survivors are not exactly one — so `for x : u16 in
  s` on a source composing `ToIterator<u8>`/`ToIterator<char>` renders
  `for-loop source has ambiguous element type: ` with nothing after the
  colon. A zero-candidate result is a *mismatch*, not an ambiguity, and wants
  its own diagnostic naming what was available.

- **The bound context is widened to every compose on the concrete type**,
  which voids the coherence guarantee `compose` exists to provide.
  `Driver::check_generic_bounds` (`omega-driver/src/items.rs`, the
  `Some(Ok(..))` arm) seeds the analyzer with the declared bound *plus*
  `composes_for_type(concrete)` — every compose anyone attached to that
  type. So inside `f<T: Speak>(x: *T)` at `T = Dog`, `x.secret()` resolves
  whenever some package wrote `compose Dog : Secret`, even though `Secret`
  was never bounded on. `analyze_for_in` (`omega-analyzer/src/analysis/
  stmts.rs`, both `composes_for_type` calls) does the same thing, narrowly,
  around its synthesized `to_iterator`/`next` calls.

  This is the case the plan's own risk list said to *flag rather than
  widen*, and it was not fixed here because removing the widening alone
  breaks a legitimate program: a **spec-alias or spec-dependency bound
  never reaches the composes that satisfy it**. `spec MySpec = Dummy |
  Mammal` with `compose Wolf : Mammal` seeds the bound context with
  `(Wolf, MySpec)`, and there is no compose entry under that key —
  `examples/dev/main.omg:514`'s `accepts_myspec<T: MySpec>` stops
  compiling. Conformance itself still succeeds, because
  `type_implements_spec`'s fallback arm signature-matches the flattened
  requirements against every compose on the type; only the *bound context*
  has no way to follow the same path.

  Resolving this means deciding how a bound expands into compose entries:
  the natural answer is the bound spec's transitive `dependencies` closure
  (`ResolvedSpecType::dependencies`, which is also how an alias is
  represented), seeded once in `check_generic_bounds`. The complication is
  that a dependency carries **raw, unresolved type arguments** by design
  (`spec Foo<T> : Bar<T>` — see `dependencies`' doc comment), so the
  closure needs `flatten_spec_into`'s deferred argument resolution rather
  than a plain walk. That is a design decision the plan did not make, which
  is why it is recorded here rather than guessed at.

  The narrower half of the same hole *was* fixed: an aggregate's own
  inherent method bodies no longer get every compose on the type
  (`omega-driver/src/bodies.rs`), which was a fourth seeding point the plan
  never listed.

- **A generic-target compose instantiated at two different `T` collides in
  codegen.** `compose<T> Box<T> : S` used at both `Box<i32>` and `Box<u8>`
  panics the compiler with cranelift's `DuplicateDefinition`. Each
  instantiation is correctly registered as its own `ComposeEntry` with its
  own target, and the two mangle to distinct symbols
  (`...Box<i32>::S::g` / `...Box<u8>::S::g`), but both bodies inherit
  `decl_id` from the *template's* HIR function and carry no `type_args` of
  their own, and `Codegen::declare_function_def` keys `self.functions` on
  exactly that `decl_id` — so the second declaration overwrites the first's
  `FuncId` and both defines land on one function.

  This is the plan's own "generic target instantiated at two different `T`"
  test case; it has no test today. Fixing it means giving each compose
  instantiation its own identity through checked/MIR/codegen the way a
  generic function's `type_args` already do — either by minting a fresh
  `decl_id` per instantiation in `check_compose_block` (which also changes
  what `type_implements_spec` hands back as vtable slots) or by carrying
  the target's type arguments in `CheckedFunctionDef` and keying on the
  pair. Both are pipeline-wide, so neither is a local fix.

  Until then, a generic-target compose is safe only when a single program
  instantiates it at one `T`. `std::list`'s `compose<T> List<T> :
  ToIterator<T>` is within that limit today.

- **`core` declares its spec-satisfying methods as inherent `primitive`
  methods**, with the compose blocks left empty (`primitive bool { exposed
  fmt(...) }` plus `compose bool : Display {}`; likewise every `numerics`
  macro, `strings`, `chars`). `check_compose_block` satisfies a requirement
  from the target's inherent methods when the compose body doesn't supply
  it, which is what makes this work. The consequence is that the plan's
  headline example — `42.fmt(w)` being rejected in favour of
  `Display::fmt(42, w)` — is **not** true for any primitive: `fmt`,
  `equals`, `compare`, `hash` and `default` are all inherent on every
  scalar, so they resolve on a bare receiver with no bound in sight. It is
  not unsound (`core` owns those types and may declare inherent methods on
  them), but it permanently occupies those names in each primitive's own
  namespace and means the negative case the plan wanted asserted cannot be
  written against `Display`. Deciding whether the bodies belong in the
  `compose` blocks instead is a migration, not a compiler change.

- **The compose/primitive registry keys on `ResolvedType` equality, which is
  finer than the identity that decides a type's method table.** This is the
  general statement of the two entries that follow; both are instances, and a
  fix should target the general form rather than either symptom.

  A `ResolvedType` carries refinements that are *not* part of "which
  behaviour does this type have": an enum's `variant`, and pointer/slice/`str`
  mutability. Every existing method-lookup path already erases them —
  `find_methods`' enum arm reads the cell's own `functions` ignoring
  `variant`, and `adapt_self_argument` re-stamps mutability to whatever the
  self-mode declares. But `Driver::compose_for`, `composes_for_type`, and
  `primitive_methods` all select with `entry.target == *target`, i.e.
  `ResolvedType`'s derived structural equality, which distinguishes exactly
  those refinements.

  The fix is a canonical **lookup key** — widen enum variants, drop
  mutability — applied on entry to all three, rather than teaching each
  caller to normalize. Deliberately *not* "route lookup through the subtyping
  relation": coercion semantics do not belong in a registry probe, and the
  two refinements are not coercions of the same kind.

- **A variant-narrowed enum never finds its own compose.** `enum Color { Red,
  Green }` with `compose Color : Show`, then `c := Color::Red; Show::show(&c)`
  fails with `'Color::Red' does not implement spec 'Show' (missing: show)`.
  Binding the same value from a function returning `Color` works, and so does
  reaching it through a generic bound (`use_it<T: Show>(&c)`) — only the
  spec-qualified call on a refined value fails.

  The sharpest form: on one binding `abc : Shape::Circle`, field access
  (`abc.r`) works, an **inherent** method (`abc.tell()`) works, and a
  **composed** method (`Show::show(&abc)`) does not. Same value, same
  refinement, two lookup paths disagreeing — so this is an inconsistency
  between two implementations of one concept, not an open question about
  variant subtyping.

  `Color::Red` has type `ResolvedType::Enum { variant: Some(0) }`; the compose
  was registered under `variant: None`, and the derived equality compares the
  field. A **regression**: the old model read `implemented_specs` straight off
  the enum's cell, where `variant` could not participate.

  **Settled, not a design decision**: conformance belongs to the enum, so the
  answer is always to widen. A refinement is a *proof* carried in the type —
  real, and load-bearing for field access and variant-typed parameters — but
  not a separate identity with a method table of its own. See
  [enums](05-enums-and-pattern-matching.md)'s refinement section, which also
  records why per-variant conformance is not wanted (an unrefined value would
  have no determinable vtable). `ResolvedType::widened()` already collapses
  `Some(_) -> None` and is already called by `adapt_self_argument` for this
  exact reason. Match-narrowed bindings (`declare_narrowed_binding`) reach the
  same shape, so this is not limited to a literal variant path.

- **Calling any `primitive<T> [?]T` method on a *mutable* slice panics the
  compiler.** `mut a: [4]u8; rw := &mut a[0..]; rw.is_empty()` aborts with
  cranelift's `DuplicateDefinition` on
  `_omg_NvNtC4main6*[?]u88is_emptyShEb`. The immutable form (`&a[0..]`) is
  fine, and the trigger is the receiver's mutability, not the method — `get`
  fails identically. In effect **`core::slices` is unusable on mutable
  slices**, which is ordinary code; no test covers it, and `just test-io`
  passes because `examples/io_demo` only ever passes mutable slices as
  arguments, never uses one as a method receiver.

  Mechanism: `match_primitive_target` destructures `ResolvedType::Slice
  { item, .. }`, ignoring mutability, so it matches a mutable slice and
  instantiates the template with the mutable target; but `primitive_methods`'
  cache probe and `instantiate_primitive`'s duplicate check both compare
  `entry.target == target` *including* mutability. Meanwhile
  `adapt_self_argument` re-stamps `*self` to the self-mode's own mutability.
  So one element type yields two entries that share every method `decl_id`
  (inherited from the template's HIR) and collapse onto one symbol at
  emission. Same "two instantiations, one `decl_id`" root cause as the
  generic-target compose collision below, but reachable without writing a
  single `compose`.

- **Symbols for primitive and compose methods on unnamed targets embed raw
  type renderings.** `omega-codegen/src/mangle.rs`'s target path falls back to
  `ManglePath::Root(target.to_string())` when the target has no declared name,
  so `ResolvedType`'s `Display` output goes straight into the symbol:
  `core::strings`' slice/`str` methods ship today as
  `_omg_NvNtNtC4core7strings4*str8is_emptyTEb`, whose path segment is
  literally `*str`, and the slice forms render `*[?]u8` and — containing a
  **space** — `*mut [?]u8`.

  The byte-length prefixes are correct, so these parse, but they leave the
  `[A-Za-z0-9_]` set that the rest of the scheme deliberately stays inside:
  `vtable_symbol`'s own doc comment explains that RFC 2603's
  `<vendor-specific-suffix>` production is *not* used precisely because
  arbitrary bytes in compiler-emitted symbols are a cross-platform
  portability problem. A space in a symbol name is exactly that problem, and
  `omg_demangle` cannot round-trip these. The fix is a real encoding for
  structural targets — the same `MangleType` grammar `mangle_type` already
  produces for every other position — rather than a `Display` fallback.

- **Slice and pointer compose/primitive targets are unreachable, in two
  different ways.** The contextual-keyword dispatch in
  `omega-parser/src/parser/item.rs` recognizes `compose`/`primitive` only when
  the next token is `Ident` or `<`, but `parse_compose_def`/
  `parse_primitive_def` both call the full `parse_type`. So the grammar those
  functions implement is strictly wider than anything the dispatcher can route
  to them:

  - `compose [?]u8 : Eq`, `compose *Foo : Eq`, `primitive [?]u8 { }` are not
    recognized as declarations at all. They fall through to the top-level
    binding path and report `expected ':', found '['`, which names nothing a
    reader can act on.
  - `compose<T> [?]T : Eq` and `compose<T> *T : Eq` *are* recognized (they
    start with `<`) and pass `blanket_parameter` (`T` does occur in the
    target), so they register as templates — but `match_compose_target` only
    handles `Type::Generic`, so no target can ever bind them. They are
    **silently dropped**, and the only diagnostic anyone sees is
    `SpecNotImplemented` at an unrelated use site. This is precisely the
    failure mode `collect_compose_signatures`' own comment says the
    `BlanketComposeNotYetSupported` error exists to prevent.

  Net effect: **a slice type can never conform to any spec.** `[?]T` can carry
  inherent methods (`primitive_target_allowed` explicitly permits `Slice`, and
  `core::slices` uses it) but can never be `Eq`, `Display`, or iterable, while
  `str` — the other fat pointer, treated identically by `adapt_self_argument`
  and by `primitive_target_allowed` — is fully composable
  (`core::strings`' `compose str : Eq {}`). Fixing this means widening the
  lookahead and teaching `match_compose_target` the non-`Generic` shapes, or
  deciding these targets are out of scope and rejecting all four spellings
  with one honest diagnostic. [specs.md](08-specs.md)

- **An extra *overload* of a required name in a compose block is silently
  dropped, and its body is never checked.** `Analyzer::check_compose_block`'s
  extra-function guard tests `!requirement_names.contains(&function.name)` —
  the name alone. A compose supplying both `show(*self) => i32` (the
  requirement) and `show(*self, k: i32) => i32` (not a requirement) is
  accepted with no `ComposeExtraFunction`, emits no symbol for the second, and
  **does not type-check its body**: `show(*self, k: i32) => i32 { "not an
  int" }` compiles clean. A differently-*named* extra is correctly rejected,
  so this is specifically the overload case. The guard needs to match on
  `(name, signature)`, the same pairing the requirement matching just above it
  already uses.

- **A `hidden` inherent method becomes publicly callable through an exposed
  compose, with nothing at its declaration to say so.** When a compose block
  omits a requirement, `check_compose_block` satisfies it from the target's
  inherent methods and overwrites the method's visibility with the
  requirement's (`method.visibility = requirement.visibility`). So a `hidden`
  method — per [visibility.md](07-visibility.md), callable only from its own
  type's method bodies — paired with `compose Foo : ExposedSpec {}` becomes
  reachable from any package as `ExposedSpec::method(&foo)`.

  Arguably intended (composing with an exposed spec is a deliberate act of
  exposure), but it is the unexamined interaction of two decisions made
  separately: the inherent fallback, and dropping per-method visibility on
  compose functions. The latter deleted `SpecMethodTooHidden`, whose entire
  job was "an implementor can never narrow a spec's contract"; the opposite
  direction — a spec silently *widening* an implementor's own declared
  visibility — is now unguarded, and is invisible at the method's declaration
  site. Worth an explicit decision either way. [visibility.md](07-visibility.md)

- **`ComposeTargetNotAType` is defined but never constructed**
  (`omega-analyzer/src/error/kind.rs`), with a rendered label and message
  that no input can reach.

- **Six code comments still describe `for`-attached specs** as a live
  mechanism (`analysis/specs.rs:319` and `:362`, `analysis/calls.rs:604`,
  `:634`, `:723`, `resolved_type.rs:1085`); `specs.rs:319` names
  `Analyzer::resolve_extension_methods`, which this change deleted.

## Specs

### Fixed in the composition/spec structural repair

- **Coercion into `spec *T` now covers every expression position** —
  struct-literal fields, array-literal elements, and a bare tail-return
  without `return`, alongside the four positions that already worked.
  [specs.md](08-specs.md)
- **A type implementing `ToIterator<T>` more than once can be disambiguated**
  with a binding annotation, `for value : u8 in source`. Unannotated, more
  than one candidate is an ambiguity error naming each `T` (but see the
  empty-candidate-list bug in the composition section above).
  [for-in-loops.md](18-for-in-loops.md)

### Still open after that change

- **`is_variadic` on spec functions is parsed and stored but never
  observed.** `spec Log { write(*self, ...) => i32; }` now parses, and
  `resolve_raw_spec_fn_type` threads the flag into the
  `ResolvedFunctionType`, but nothing reads it: the arity checks for a
  spec-qualified call (`analysis/calls.rs:302`) and for spec-object dispatch
  (`analysis/calls.rs:1041`) still compare `args.len()` against
  `params.len()` unconditionally, so `Log::write(&l, 1, 2)` is rejected as
  "takes 1 argument but 3 were supplied". Separately, Omega has no way to
  *define* a variadic function (see [functions.md](00-functions.md)), so a
  compose block cannot declare `...` at all and no variadic requirement is
  implementable. Making this real means deciding whether variadic spec
  functions exist to be *called* (extern/glue-shaped) or to be
  *implemented*, and wiring the arity checks accordingly.
  [specs.md](08-specs.md)
- **`spec T` return-type inference on a method observes a partially
  populated owner cell.** `collect_methods` now probes a method's body when
  its return type is bare `spec T`, which runs while the owning type's cell
  is still `InProgress` — so the body can see the type's *fields* but not
  its *methods*. `exposed pick(*self) => spec Animal { self.helper() }`
  fails with `no field 'helper' on 'Zoo'` regardless of declaration order.
  This is the exact hazard the plan flagged; it shipped rather than being
  abandoned. Either the probe needs the method table populated first (which
  is what makes this hard — signatures are what `collect_methods` is
  computing), or the diagnostic must at minimum say *why* siblings are
  invisible instead of claiming the method is a missing field. Overloaded
  free functions remain outside the rule as before. [specs.md](08-specs.md)

## Gaps and glue

- **No default-bodied `gap` function** — every gap function must
  currently be a bare requirement; a body is rejected outright
  ([gaps-and-glue.md](21-gaps-and-glue.md)).
- **No "override" or test-only glue concept** — a second `glue` for the
  same gap is always a hard error project-wide, with no way to shadow one
  intentionally. [gaps-and-glue.md](21-gaps-and-glue.md)
- **`MultipleGluesForGap` cannot point at the conflicting glue blocks.**
  The error is anchored at the *gap*'s declaration (correctly — neither
  glue is more at fault), and names each conflicting glue as
  `<module path>#<internal HirId>`, e.g. `plat#1, other#1`. Within a single
  module that degrades to `t#3, t#7`, which names nothing a reader can act
  on. The real fix is a secondary diagnostic label at each glue's own span,
  and those spans are in *different files* from the primary — the renderer
  only supports same-file secondary labels today (`Redeclaration`'s
  `previous: Option<Span>` is the only precedent). Resolving it means
  either cross-file labels in `omega-diagnostics`, or having
  `Driver::sweep_gaps` emit one additional `CompileError::Analysis` per
  glue site in that glue's own module. Left alone because the choice
  between those is a diagnostics-subsystem design decision, not a local fix.
- **`@suppress(unfilled_gap)` is unreachable.** Every warning's rendering
  ends with the generic "suppress this with `@suppress(<slug>)`" note, so
  `UnfilledGap` advertises it — but `gap` is a first-class declaration that
  takes no annotations at all, so following the advice is now a hard parse
  error. It never worked before either: `Driver::sweep_gaps` constructs the
  warning directly rather than going through `Analyzer::warn`, which is the
  only thing that consults `@suppress`. Fixing it means either giving
  `HirGapDef` an annotation list (which the gap/glue plan deliberately
  avoided, to keep anything downstream from branching on gap-level
  metadata) or teaching the whole-program sweeps to honour suppression and
  suppressing the note when a warning kind has no suppressible anchor.
  [gaps-and-glue.md](21-gaps-and-glue.md)

## Visibility

- **No re-export / `pub use`-equivalent.** Matches the language having no
  re-export concept at all today. [visibility.md](07-visibility.md)

## Macros

- **A node built from a macro expansion gets a composite span running from
  the call site to the definition site, and statement position makes it
  visible in ordinary diagnostics.** Every token keeps its own real
  originating span (deliberately — there is no render-to-text-and-relex
  round trip), so a node built from a mix of call-site argument tokens and
  definition-site body tokens spans both. `expand_expr` hides this for
  expression position by pinning the invocation's own call-site span back
  onto the outer node, but a statement-position expansion has no equivalent:
  the spliced statements and their expressions keep the composite spans the
  re-parse produced. `just build-exe` on `examples/dev/main.omg`
  demonstrates it — the `unused return value` warning for
  `call_each$(puts, ...)` at line 769 renders a label stretching to the
  macro's own definition at line 1495, ~700 lines of elided snippet for a
  one-line statement. Not fixed here because the honest fix is a single
  span policy shared by all three positions (item position has the same
  latent problem), which is a design decision rather than a local patch:
  either remap every span in an expansion to the call site (losing the
  ability to point inside a macro body at all) or give `Span` a notion of
  expansion provenance so the renderer can show both. Pinning only the
  top-level statement node would fix the demonstrated case while leaving
  every nested expression inside an expansion still wrong.
  [macros.md](12-macros.md)
- **`MAX_EXPANSIONS` does not actually prevent the stack overflow it
  documents.** `macros.rs`'s budget is spent one unit per invocation and
  reports `ExpansionLimitExceeded` at 256, but each expansion costs roughly
  twenty stack frames (the recursive-descent re-parse plus `expand_expr`'s
  own very large frame), so `macro a() => { a$() }` aborts on a stack
  overflow before the budget runs out on a 2 MiB thread stack — it only
  reports cleanly with `RUST_MIN_STACK` raised. Pre-existing: reproduced
  identically on the baseline commit with the old `a!()` syntax. Statement
  position adds a second recursion path (`expand_statements_invocation`)
  with the same shape. The fix is a *depth* limit rather than (or as well
  as) a total-expansion budget. [macros.md](12-macros.md)
- **Duplicate macro parameter names are silently accepted**, e.g.
  `macro m($a: expr, $a: expr)`; bindings are a `HashMap`, so the later
  parameter wins and the earlier one becomes unreferenceable. The same
  applies when a fixed parameter and the variadic share a name, where the
  variadic's `Many` binding shadows the fixed `One`. Pre-existing (the flat
  `Vec<MacroParam>` model had it too); the fix is one duplicate check in
  `parse_macro_signature` plus a `ParseErrorKind` variant.
  [macros.md](12-macros.md)
- **A repetition separator is not restricted to tokens that can survive
  substitution.** `parse_repetition` only rejects brackets and multi-token
  separators, so `$...($x){ ... }` or `$...($){ ... }` parses, emits the
  `$name`/`$` token literally, and fails much later with a confusing
  expansion-site parse error rather than at the definition. Low impact
  (nobody writes it deliberately), but the diagnostic points at the wrong
  place. [macros.md](12-macros.md)
- **Macro visibility is not transitive.** A module's macro environment is
  built from its *own* import statements and each target's *own* definitions;
  an imported module's imports are never followed. This matches the language
  having no re-export concept, and it is what keeps the pre-pass acyclic, but
  it means a package cannot curate a macro surface the way it can't curate an
  item surface. [macros.md](12-macros.md), [visibility.md](07-visibility.md)
- **A macro body's nested invocations resolve at the call site, not the
  definition site**, because expansion is textual splicing into the caller's
  environment. A macro exported from one package therefore cannot call a
  helper macro that the caller cannot also see, and the resulting
  `UnknownMacro` names the *inner* macro with no indication that it came from
  an expansion. The fix is a per-definition home environment carried
  alongside each `MacroDefinitionStmt`. [macros.md](12-macros.md)
- **Importing a macro leaves a spurious `unused import` warning.** Macro
  names are resolved and consumed by the pre-pass in `omega-driver`'s
  `Driver::macro_env`, entirely before HIR exists, so the ordinary
  import-usage tracking never observes the use and reports the import as
  dead. Every cross-package macro import warns today.
  [macros.md](12-macros.md), [visibility.md](07-visibility.md)
- **A macro-generated `import` contributes no macros.** Item-position
  expansion can emit a real `Item::Import`, but macro resolution already ran
  against the file's hand-written imports by then. Deliberate and documented,
  not a bug to fix locally — the alternative is expanding to a fixpoint.
  [macros.md](12-macros.md)
- **A statement-position expansion's locals capture caller expressions of the
  same name.** The expansion is spliced into the caller's block, so an
  argument naming a caller variable binds to a macro-introduced local that
  shadows it. Wrapping the body in `{ }` prevents the local from leaking out
  but not from capturing in — that block is the scope the argument lands in.
  Hit for real: `core::io`'s print macros originally named their writer `out`,
  which silently shadowed the `*str out` in `examples/dev/main.omg` and made
  `println$(..., out)` fail with `no field 'fmt' on 'Writer'`. Worked around
  with an `omega_print_` prefix, which is a convention, not a guarantee. A
  real fix is gensym or a hygiene scope. [macros.md](12-macros.md)

## Compiler internals

Shape problems in `omega-driver` and `omega-analyzer` that work today but each
need a breaking change to fix — full writeups in
[design-review.md](17-design-review.md#compiler-architecture).

- **Overloading needs a whole parallel item pipeline** (two extra caches,
  two extra sweeps, two extra resolver methods) purely because the item
  query key can't name one candidate of an overload group — which also
  makes generic overloads structurally impossible. Confirmed: a generic and
  non-generic overload of the same name (`f(x: i32)` / `f<T>(x: T)`) doesn't
  just get rejected, it fails with an opaque, rootless diagnostic
  (`ResolveError::ItemFailed` firing with no primary error ever shown) —
  likely `ensure_overload_signature` resolving a generic candidate's own
  signature with an empty substitution list.
- **Two independent pending-spec-method queues** that differ only in
  whether the owner has a declared item to key on.
- **`core` is the only package allowed to declare `primitive` blocks**, so
  third-party packages cannot add inherent methods to built-in types. They
  can compose specs under the target-or-spec-local orphan rule.
- **`ResolveError::Cycle` carries a chain it never populates** — it always
  prints one module, so the rendered message implies a cycle it never
  shows.
- **Module paths and item paths are the same untyped `Vec<Ident>`**, so
  nothing prevents confusing the two.
- **Diagnostic scoping for scanned (extern/`core`) modules is three ad-hoc
  lists** with four different outcomes and no stated policy.
- **A node's identity is threaded as a bare `(HirId, Span)` pair** through
  ~60 analyzer signatures, with nothing tying the two together.
- **`reveal`'s bypass must be re-activated by every operand position
  individually**, with no backstop — three positions have now been fixed
  one at a time.

## Design debt worth watching

- Every new `Expression`/`HirExpr`/`CheckedExpr` variant needs updates
  across up to five separate exhaustive matches spread over multiple
  crates (macro expansion, prelude re-exports, HIR lowering, defer-id
  collection, codegen emission) — the compiler catches every miss as a
  hard exhaustiveness error, so nothing is silently skipped, but budget
  for it when adding new expression forms.

## Diagnostics

- **A `*mut self` requirement against an rvalue receiver reports
  `NotMutablePointer`, and the invariant that made that correct no longer
  holds.** `Bump::bump(make())`, where `bump(*mut self)` and `make()` returns
  by value, is correctly *rejected* — the mutation would land in a temporary
  that is immediately discarded — but the message is `cannot mutate through
  an immutable pointer`, naming a pointer that does not appear in the source
  and that no added `mut` can fix.

  `Analyzer::require_mutable_place` (`analysis/places.rs`) picks between
  `NotMutableBinding` and `NotMutablePointer` by asking whether the checked
  place has a `Deref` projection, falling through to `NotMutablePointer` for
  any root that isn't an unqualified path. Its own doc comment states the
  assumption that justified that fallback: "a non-place root, e.g. a
  freshly-constructed value, is never itself the *cause* of immutability --
  something dereferenced along the way always is." **That is no longer
  true.** Spec-qualified calls now wrap a non-place receiver in
  `HirPlaceRoot::Expr` so it can be adapted at all (see
  `Analyzer::adapt_self_argument`), producing a place with a non-path root
  and *no* `Deref` projection — the exact shape the assumption excluded.

  Pre-existing in the sense that a receiver-position call on an rvalue
  reaches the same arm, but newly *reachable* in ordinary code: a
  spec-qualified call is the normal way to invoke a composed method, and its
  receiver is an ordinary argument expression that anyone may write as a call
  or a literal. The fix is a third `AnalysisErrorKind` for the not-a-place
  case — "`*mut self` needs a place to mutate; bind the value to a `mut`
  local first" — selected in `require_mutable_place` before the
  `through_pointer` test, plus a correction to that doc comment.
  [specs.md](08-specs.md)
- **Taking a slice of a local array doesn't count as a use of that array**,
  so `mut b : [8]u8; s := &mut b[0..]; s[0] = 1u8;` warns `unused variable`
  for *both* `b` and `s`. Writing through a slice isn't tracked as a read of
  its base either. Previously obscure; now unavoidable, because
  `core::io`'s `println$`/`print$`/`eprint$`/`eprintln$` all declare a
  `mut buf : [256]u8;` in their expansion, so **every print statement emits a
  spurious `unused variable 'buf'`** — a hello-world program warns. Compounded
  by the composite-span issue below, the label points past the end of the
  file. The fix is in the analyzer's use-tracking (a slice/`&mut` of a place
  is a use of that place), not in `core::io`.
  [console-io.md](24-console-io.md)

## Control flow

- **A bare `return;` is a parse error**, so a `void` function cannot return
  early at all — `expected an expression, found ';'`. Every early exit in a
  `void` body has to be restructured around a sentinel flag, which
  `core::io::Writer::write_bytes` and `std::io::read_line` both had to do.
  [control-flow.md](03-control-flow.md)
