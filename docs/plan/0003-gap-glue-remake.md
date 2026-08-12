# `gap` and `glue` as first-class declarations

> **Sequencing note.** This plan replaces an earlier one that reworked Omega's whole
> composition model (`compose`/`primitive`, removal of declaration-site conformance,
> deletion of `spec ... for`). That work is still intended and comes next — see
> "Follow-up work" for the decisions already settled, so they are not re-litigated. This
> plan is deliberately first because it *removes* glue from that change's scope rather than
> deferring it: once glue no longer uses declaration-site conformance,
> `resolve_implements_clause` becomes purely about type-to-spec conformance before the
> composition rework rewrites it.

## Task Description

- **What is being asked:** Promote `@gap` and `@glue` from annotations on `spec`/`marker`
  to their own declaration forms, and stop modelling a gap as a spec.

  ```omega
  # before
  @gap
  exposed spec GlobalAllocator {
      alloc(size: usize) => *mut u8;
  }

  @glue
  exposed marker LibcAllocator : core::glue::GlobalAllocator {
      exposed alloc(size: usize) => *mut u8 { return malloc(size); }
  }

  # after
  gap GlobalAllocator {
      alloc(size: usize) => *mut u8;
  }

  glue core::glue::GlobalAllocator {
      alloc(size: usize) => *mut u8 { return malloc(size); }
  }
  ```

- **Purpose:** A gap is a **signature** — a named set of function declarations with no
  implementation, filled exactly once for the whole program at link time. A glue is the
  **structure** that fills it. Neither is a relationship between a type and an abstraction,
  which is the only thing `spec` is for.

  Forcing them into `spec`/`marker` costs real machinery whose only job is to take back
  what spec-ness wrongly grants. Serves *no hidden behavior* (the declaration states the
  concept) and *modern syntax with real abstraction power* (one construct per concept).

- **Reasoning:**

  **The gap side.** Four of the five `@gap` diagnostics exist solely to un-spec a spec, and
  their own doc comments say so:

  | Diagnostic | Its stated rationale |
  |---|---|
  | `GapFunctionMustBeStatic` (`kind.rs:552`) | "there's no instance to hang a `self` off of" |
  | `GapOnForSpec` (`:568`) | "no implementor concept at all" |
  | `GapOnSpecAlias` (`:571`) | "an alias has no function list of its own" |
  | `GapMustNotBeGeneric` (`:578`) | "computed once, for the bare, ungenericized spec" |

  A `gap` declaration has no `self`, no `for` target, no alias form and no generic list in
  its grammar, so all four become unreachable rather than enforced.

  **The glue side.** The marker is a fiction. Gap functions are self-less —
  `docs/21-gaps-and-glue.md` says they are "called through this spec's own qualified name
  ... never through an instance" — so no glue marker is ever constructed, addressed, or
  passed anywhere. Verified: `plat`'s four markers appear nowhere in `runtime/`,
  `examples/`, or `tests/` except at their own declarations. Three more diagnostics exist
  only to police that fiction: `GlueOnNonMarker` (`:582`), `GlueMustNotBeGeneric` (`:588`),
  and `GlueOnNonGapSpec` (`:595`).

  **Seven diagnostics deleted in total**, because the grammar expresses the constraint
  instead of an annotation policing it after the fact.

  It also removes a documented workaround. `runtime/plat/libc/libc.omg:36` explains that
  `StandardOutput::write` and `StandardError::write` "deliberately have the same method
  signature. Current glue lowering exports one symbol per marker method, so each stream
  needs its own marker to preserve both gap symbols."

  That explanation is subtly wrong, and the correction is why the workaround dies:
  `ManglingMode::Glued` already carries `{ spec_module_path, spec_name, function_name }`
  (`compile.rs:421-425`), so the *symbols* were never in conflict. The real conflict is that
  two methods named `write` on one *type* are a `Redeclaration`. Remove the type and the
  collision cannot occur — one glue block per gap becomes the natural shape.

  Alternatives considered:

  - *Promote `glue` only, keep `@gap spec`.* Rejected: they are one mechanism. Splitting
    them would touch `annotations.rs`, the diagnostics, and `sweep_gaps` twice, and would
    leave the gap side still pretending to be a spec.
  - *Keep `@glue` as an annotation on a new block.* Rejected: annotations attach to
    declarations, and the block *is* the declaration.
  - *`@glue compose Marker : Gap`* (the mechanical rewrite under the coming composition
    rules). Rejected: preserves the fiction and adds ceremony.
  - *Keep a gap as a spec but give it a dedicated keyword.* Rejected: `is_gap` branches
    would remain scattered through spec resolution, which is most of the cost.

- **Resolved concerns:**
  - **Is a gap still a spec?** No. It becomes its own item kind: not a type, never usable as
    a bound (`T: Gap` is meaningless with self-less functions) and never as `spec *Gap`
    (there is no instance). It is a path-qualified namespace of functions.
  - **Does `marker` survive?** Yes, for `Unit` as a zero-sized placeholder in `HashSet<T>`
    over `HashMap<T, Unit>`, and for stateless singletons implementing ordinary
    `*self`-taking specs. It just stops being load-bearing for glue.
  - **Default-bodied gap functions** (`GapFunctionBodyNotYetSupported`, `:563`) are a real
    unimplemented feature, not a shape rule. Under `gap`, the grammar simply does not accept
    bodies, so "not yet supported" becomes the honest "a gap declares, it does not define."
    Adding fallback bodies later remains possible and is out of scope here.

    **Carry the existing rationale into the new diagnostic** rather than emitting a bare
    syntax error. The current one explains *why* — a default-bodied gap function needs a
    real, once-compiled MIR function reusing the synthetic-`HirFunctionDef` reconstruction
    machinery that `Analyzer::check_pending_spec_method` already needs — and that is exactly
    what someone hitting this wants to know. Losing it would be a diagnostic regression
    dressed up as a simplification.

## Technical Details

### The two forms

```
gap <Name> {
    <function declarations — no bodies, no self, no generics, no visibility>
}

glue <qualified-gap-path> {
    <function definitions — no self, no generics, no visibility>
}
```

**Neither form carries a visibility modifier, and both are implicitly global.** This is a
deliberate departure from `spec`, and the reason is that neither is a name-level
declaration:

- A **glue** is never named by anyone. The compiler finds it through its gap; the linker
  resolves it.
- A **gap**'s visibility would not control name access in any meaningful sense — it would
  control *who is responsible for filling it*, and that is global by construction.
  `ManglingMode::Glued` computes one fixed linker symbol program-wide, and
  `MultipleGluesForGap` is a whole-program check that already ignores declared visibility.

A non-exposed gap is therefore incoherent: a `hidden` gap that nothing glues produces an
undefined symbol at final link that nobody outside the declaring module is permitted to fix.
Supporting it would require a second rule — "a non-exposed gap must be glued within its own
scope" — giving two kinds of gap with different semantics to serve a case that does not
exist. All four gaps in `runtime/core/core/glue.omg` are `exposed` today; there are no
counter-examples in the tree.

This is also the reversible direction: forbidding the modifier now and adding it later, if a
genuine case for a package-private platform seam appears, is backward compatible. Allowing
it now and discovering it is meaningless is not.

A `spec`, by contrast, keeps its visibility — it is named in bounds, in types, and in
`compose` blocks, so it has a real API surface to control. The difference reinforces that a
gap is not a spec.

### What changes

**Parser** (`omega-parser`)
- New `gap` statement: name, `{ }` of function *declarations*. Reject bodies, `self`
  parameters, generic lists, **and visibility modifiers** at parse time.
- New `glue` statement: qualified path, `{ }` of function *definitions*. Reject `self`,
  generics, and visibility modifiers at parse time.
- Both are **contextual keywords recognized only at item position**, matching
  `exposed`/`internal`/`reveal` (identifier text, not reserved words — see
  `docs/07-visibility.md`).

  Item position is the only place they are recognized, which removes most of the collision
  surface by construction: a local named `gap` or `glue` lives in *statement* position
  inside a function body, which this grammar never reaches. Mid-path occurrences are equally
  safe — `import core::glue::GlobalAllocator;` and
  `core::glue::GlobalAllocator::alloc(...)` never place `glue` first in an item.

  The one case needing lookahead is a top-level binding, since `HirItem::Declaration` /
  `DeclarationWithInit` / `Walrus` all let an item start with a bare identifier:

  ```omega
  gap := 5;                    # global named `gap`
  gap GlobalAllocator { ... }  # gap declaration
  ```

  One token resolves it: `:` or `:=` means a binding, an identifier means a declaration.
  This is not new machinery — the parser already does exactly this for `exposed`, which is
  contextual and therefore also legal as a global name (`exposed := 5;` versus
  `exposed struct Foo { }`). Follow that existing path rather than inventing a second one.
- Delete `@gap`/`@glue` annotation parsing.

**HIR** (`omega-hir/src/hir.rs`) — new `HirGapDef { id, span, visibility, name, functions }`
and `HirGlueDef { id, span, gap: Path, functions }`.

**Annotations** (`omega-analyzer/src/annotations.rs:176-186`) — delete the `gap: bool` and
`glue: bool` fields and both match arms.

**Analyzer** (`omega-analyzer`)
- Gaps become their own resolved item, not a `ResolvedSpecType`. Delete `is_gap`
  (`resolved_type.rs:351`), its `GapFunction` list (`:338`, documented as "Empty unless
  `is_gap`"), and both assignment sites (`specs.rs:446`, `omega-driver/src/items.rs:926`).
- `paths.rs:455` — the `ResolvedType::Spec(cell) if cell.borrow().is_gap` arm becomes a
  dedicated gap-path arm. This gets *simpler*: a gap is a function namespace, so
  `Gap::function(...)` is a plain path resolution rather than a synthetic `ResolvedMethod`
  built from a spec's signature.
- New glue checker: resolve the path, require a gap, check the function set against the
  gap's own (missing / extra / mismatched signature), set `ManglingMode::Glued`.
- `resolve_implements_clause` (`specs.rs:871`) — **drop the `glue: bool` parameter** and the
  `glue && !spec.is_gap` check at `:889`. It becomes purely type-to-spec conformance.
- `items.rs:604-623` — delete the glue mangling block; it moves to the glue checker.
  `items.rs:643` — delete `cell.borrow_mut().is_glue`; delete the field (`items.rs:136`).
- `error/kind.rs` — delete `GapFunctionMustBeStatic`, `GapOnForSpec`, `GapOnSpecAlias`,
  `GapMustNotBeGeneric`, `GlueOnNonMarker`, `GlueMustNotBeGeneric`, `GlueOnNonGapSpec`.
  Replace `GapFunctionBodyNotYetSupported` with a parse-level "a gap declares, it does not
  define". **Keep** `MultipleGluesForGap` (`:603`) and `UnfilledGap`.

**Driver** (`omega-driver`)
- `compile.rs:386` (`synthesize_gap_items`) — iterate gap declarations directly. The
  current filtering that skips `for`-specs and aliases (`:389-398`) disappears, since only
  gaps are scanned.
- `compile.rs:247-269` (`sweep_gaps`) — today it iterates `items.spec_cells()` filtering
  `is_gap`, then scans every resolved struct for `s.is_glue && implements_this_gap`. Replace
  with a lookup keyed on gap identity over glue blocks in the local package and every
  registered extern. Same `UnfilledGap` / `MultipleGluesForGap` outcomes, no struct scan.

  **Preserve the error-routing behaviour**, which is deliberate and easy to lose in a
  rewrite: `sweep_gaps` returns `CompileError`s *directly*, bypassing `Diagnostics`'
  per-module scope filtering (`drain_errors`) entirely, because a gap/glue conflict is a
  whole-program fact belonging to neither side's module — see its own doc comment at
  `compile.rs:236-240`. Routing these through the ordinary diagnostics path would silently
  drop them in compilations that do not import the offending module.
- The eager cross-module glue sweep documented at `compile.rs:170-201` must keep working: a
  glue in an unimported extern module still counts. That comment records a real past bug —
  re-read it before touching the sweep.

**Codegen / mangling** — no change. `ManglingMode::Glued` already carries the gap's
coordinates, so symbols are byte-identical **across the syntax change**. This is why it is
low-risk, and it is verified with `nm`, not assumed.

Note that `glued_symbol` (`omega-codegen/src/mangle.rs:153`) takes the module path as part
of the symbol, so the Step 0 rename *does* change all four glued symbols — by design, and
only in the module segment. Doing the rename first and re-baselining is what keeps the
byte-identical check available for the part that actually needs it.

**Runtime**
- **Module rename, done first and on its own (Step 0):** `runtime/core/core/glue.omg` →
  `runtime/core/core/platform.omg`, so `core::glue::GlobalAllocator` becomes
  `core::platform::GlobalAllocator`.

  The module holds gap *declarations*, so naming it `glue` is backwards once both sides are
  first-class. `platform` names the domain rather than either side of the relationship,
  which avoids the fact that the same declaration reads as a gap to whoever fills it and as
  glue to whoever consumes it.

  It also removes the keyword collision from first-party code entirely — after the rename
  there is no `core::glue` path at all. The contextual-keyword handling is still required
  for *user* code that happens to use `glue` as an identifier, so nothing in the Parser
  section changes.

  Call sites: `runtime/core/core/io.omg:5,7,8` (`import glue::X` → `import platform::X`),
  `runtime/plat/libc/libc.omg` (4 declarations + the header comment at `:2`), and
  comment-only references in `runtime/std/std/{alloc,list,linked_list}.omg`.
  `examples/dev/main.omg:1254` calls bare `GlobalAllocator::alloc` through the ambient
  prelude and needs **no** change.
- `runtime/core/core/platform.omg` — four `@gap exposed spec` become `gap` (the `exposed`
  modifier is dropped, not carried over; gaps are implicitly global).
- `runtime/plat/libc/libc.omg` — four `@glue exposed marker` become four `glue` blocks;
  delete the separate-markers comment at `:36`.

**Docs** — `21-gaps-and-glue.md`, `22-platform-glue.md` (every example),
`09-annotations.md` (delete `@gap`/`@glue`), `20-marker-types.md` (drop glue as a motivating
use), `24-console-io.md` (delete the separate-markers caveat), `08-specs.md` (specs no
longer have a gap mode).

`21-gaps-and-glue.md` is a **genuine rewrite, not a find-and-replace.** It currently
explains the whole mechanism in spec-and-marker terms — "a spec declares no code of its own
otherwise", "treated as if it were an ordinary marker with a static method", the implements-
clause wiring — and the "a gap is not a spec" model changes the *explanation*, not just the
syntax in the examples. Substituting keywords into the existing prose will leave it
internally inconsistent.

### What must not change

- **Symbol names.** `ManglingMode::Glued`'s computed symbol must be identical before and
  after. This is a pure front-end change.
- **Gap semantics**: self-less functions, qualified-path calls, the synthesized
  `Linkage::Import` (`compile.rs:416`), and the rule that an unglued gap links fine if
  nothing calls it.
- **Exactly one glue per gap, program-wide.**
- **Gap signatures.** The `written: *mut usize` out-pointer plus `bool` shape stays; moving
  the console gaps to `Option<usize>` belongs with the stdio rework.
- **`marker`** as a type kind, including its `implements` clause — that is the composition
  rework's business.
- **`resolve_implements_clause`'s ordinary conformance behaviour** — only the glue parameter
  and its one check are removed.

### Chosen approach

Add both declarations, migrate `core` and `plat`, then delete the annotations and their
machinery — three stages, each buildable, rather than a simultaneous swap. The migration is
eight declarations across two files, so the window where both forms exist is short.

Modelling a gap as its own item rather than a flagged spec is what makes the deletions
possible: `is_gap` currently branches through spec resolution in six places, and each is a
place where a gap has to be told it is not really a spec.

### Risks and open questions

- **Keyword collisions are handled, not open.** `glue` coexisting with the existing
  `core::glue` module is covered by item-position recognition plus the one-token lookahead
  described under Parser, on the same shape `exposed` already uses. It needs parse tests,
  not a design decision. Flag it only if the existing `exposed` lookahead path turns out not
  to generalise.
- **Gap path resolution without `ResolvedType::Spec`.** `paths.rs:455` is the one place a
  gap is reached today. If removing gaps from the type system turns out to break a
  resolution path not visible from that call site, flag it rather than reintroducing
  `is_gap`.
- **Extern-visible gap and glue discovery** must both keep working across `--extern`
  boundaries — `plat` glues `core`'s gaps, and neither package imports the other's module
  directly in the ordinary sense.
- **Diagnostic quality.** Seven diagnostics are being deleted on the grounds that the
  grammar makes them unreachable. That claim must be tested — each deleted case needs a
  negative test proving the parse-level error is at least as clear as the analysis-level one
  it replaces.

## Implementation Plan

0. **Rename the module, alone.** `runtime/core/core/glue.omg` →
   `runtime/core/core/platform.omg`; update the three imports in `core/io.omg`, the four
   qualified paths and header comment in `plat/libc/libc.omg`, and the comment-only
   references in `std/{alloc,list,linked_list}.omg`. No syntax changes. Build core, plat,
   std; run `just test-io` and both examples.

   Capture `nm target/plat.o` and `nm target/core.o` **before and after**: symbols must
   differ *only* in the `core::glue` → `core::platform` module segment, and in no other way.
   Then save the post-rename output as the baseline for Step 6.

1. **Parse and lower `gap`.** Contextual keyword, name, declaration block. Reject bodies,
   `self`, generics, and visibility modifiers at the syntax level. Add `HirGapDef` — note it
   carries **no** visibility field, so nothing downstream can start branching on one. Parser
   tests only.
2. **Parse and lower `glue`.** Contextual keyword at item position, qualified path,
   definition block. Reject visibility, `self`, and generics. Add `HirGlueDef`. Reuse the
   existing `exposed` item-position lookahead rather than adding a parallel mechanism, and
   confirm every `core::glue::...` path form still parses.
3. **Resolve gaps as their own item.** Register gap declarations in the item namespace;
   give `Gap::function(...)` its own resolution arm; make `synthesize_gap_items` iterate gap
   declarations. Both the old `@gap spec` and the new `gap` work at this point.
4. **Analyze `glue` blocks.** Resolve the target, require a gap, check the function set,
   set `ManglingMode::Glued`. Both glue forms work.
5. **Rewrite `sweep_gaps`** to key on gap identity over glue blocks, preserving the eager
   cross-module behaviour at `compile.rs:170-201`.
6. **Migrate the runtime.** `runtime/core/core/platform.omg` to `gap`;
   `runtime/plat/libc/libc.omg` to `glue`; delete the `:36` comment. Build core, plat, std;
   run `just test-io` and both examples. **`nm target/plat.o` must be byte-identical to the
   Step 0 post-rename baseline** — this is the acceptance test for the plan. The syntax
   change is front-end only; if a single symbol moved, stop.
7. **Delete the old path.** `annotations.gap`/`annotations.glue` and both match arms;
   `is_gap` (`resolved_type.rs:351`, `specs.rs:446`, `items.rs:926`) and the `GapFunction`
   list; `is_glue` (`items.rs:136`, `:643`); the glue block in `items.rs:604-623`; the
   `glue: bool` parameter and `:889` check in `resolve_implements_clause`; and the seven
   diagnostics listed above.
8. **Docs**: `21`, `22`, `09`, `20`, `24`, `08`.

## Testing

**New cases:**
- A gap declared and glued in the same package; a gap in one package glued from another
  (the `plat` → `core` shape).
- Two gaps with identical function signatures (`StandardOutput::write`,
  `StandardError::write`) glued in the same file. This is the regression test that the
  separate-markers workaround is genuinely gone.
- A gap with no glue links when nothing calls it; still fails at link when something does.
- `import core::platform::GlobalAllocator;` plus a qualified `...::alloc(...)` call.
- A *user-defined* module named `glue`, imported and used through a qualified path in the
  same file as a real `glue` declaration — the first-party collision is gone after the
  rename, so this is now the only thing exercising the contextual keyword against a real
  module name.
- Top-level bindings named `gap` and `glue` (`gap := 5;`, `glue : i32 = 5;`) coexisting in
  the same file with real `gap`/`glue` declarations — the item-position lookahead case.
- A local variable named `gap` inside a function body, confirming statement position is
  untouched by the item-position keywords.

**Negative cases** — one per deleted diagnostic, each asserting the replacement is at least
as clear:
- A gap function with a body → "a gap declares, it does not define."
- A gap function taking `self` → rejected at parse, naming the function.
- `gap Foo<T> { }` → rejected at parse.
- `glue SomeOrdinarySpec { }` → "target of a `glue` block is not a gap", naming it.
- A glue missing a required function, or declaring one the gap does not have → names it.
- `exposed glue Gap { }` and `internal gap Foo { }` → rejected at parse, each stating that
  gaps and glues are global by nature and take no visibility modifier.
- A gap declared in one package, called from a second and glued from a third, with no
  `exposed` anywhere → links and runs. This is the positive case proving implicit globality
  actually works across `--extern` boundaries, not just that the modifier is rejected.
- Two glue blocks for one gap → `MultipleGluesForGap`, naming both.

**Regression risk:**
- `nm target/plat.o` must be byte-identical between the Step 0 post-rename baseline and the
  end of Step 6. If not, stop — the syntax change is front-end only. (The Step 0 rename
  itself intentionally shifts the module segment of all four glued symbols and nothing else.)
- All 20 `core::glue` references across `docs/{10,13,21,22,23,README}.md` updated; a stale
  one is a silent documentation bug, not a build failure, so grep for the old path at the end.
- `tests/io_demo.expected` byte-identical.
- `examples/dev/main.omg:1254` calls `GlobalAllocator::alloc`/`free` directly — the clearest
  end-to-end check that gap calls still resolve.
- `compiler/omega-parser/tests` — two new statement forms and contextual-keyword handling.
- `compiler/omega-mangle/tests/roundtrip.rs` should be untouched; if it needs changing,
  something went wrong.

**Target coverage:**
- *Hosted:* `just build-core`, `build-plat`, `build-std`, `test-io`, both examples.
- *No-glue:* a program registering `core` but not `plat`, calling no gap — must link with
  only an `UnfilledGap` warning.

## Follow-up work (not this plan)

The composition rework comes next. These decisions are settled and should not be re-opened:

- **`compose T : Spec { }`** is the only way a type satisfies a spec; declaration-site
  conformance (`struct Foo : Spec`) is removed; `spec ... for` is deleted along with
  `omega-driver/src/extensions.rs`.
- **`primitive`** (in `core` only) gives built-in types a declaration site and inherent
  methods, replacing `spec ... for`.
- **Orphan rule:** a `compose` is legal iff the target type **or** the spec is declared in
  the composing package (package granularity).
- **Namespace rule:** composed methods live in the *spec's* namespace, never the type's — a
  receiver call always resolves against the type's own declaration. This removes any need
  for a method-resolution priority rule, disambiguation syntax, empty-conformance blocks,
  and per-method visibility on composed methods.
- **Blanket composes with specialization are in scope**, specificity defined as
  substitution-instance plus bound-implication over the transitive spec-dependency closure,
  with non-comparable overlaps a declaration-time error.
- **Consequence:** `core` keeps `option`, `iterator`, the gaps, and `primitive`
  declarations; `cmp`, `default`, `hash`, `fmt`, `io` move to `std`, whose primitive
  conformances are written as `compose i32 : Display` — legal because `std` owns `Display`.
