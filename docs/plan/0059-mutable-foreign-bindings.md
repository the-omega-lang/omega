# Mutable foreign bindings (`foreign mut name : T;`)

## Task Description

- **Deliverable:** `foreign mut name : T;` (and `mut name : T;` as a `foreign` block entry)
  parses, checks, and makes the bound external data symbol a **mutable place** for Omega
  code: assignment, compound assignment, `++`/`--`, and `&mut` all work on it, across
  modules and separately compiled packages. `mut` is rejected on every foreign *function*
  form. Documentation and conformance coverage land with the behavior.

- **Purpose:** today `analyze_foreign_binding` hardcodes `mutable = false`, so an external
  data symbol can only be read. Any Omega program that owns or shares mutable state with a
  C object, a platform fragment, or another separately compiled Omega object has to route
  around the language through a pointer-returning accessor. That is exactly the "no hidden
  behavior / first-class C interop" boundary Omega claims, so the missing `mut` is a real
  gap, not a convenience.

- **Chosen direction:** `mut` goes **directly before the identifier**, after `foreign` and
  after any visibility — `shared foreign mut counter : i32;`. This is the same position
  `mut` already occupies in `global-declaration` (`[ visibility ], [ "mut" ], identifier,
  ":", type`), so a `foreign` block entry becomes literally the global-declaration shape
  minus the initializer: `[ visibility ], [ "mut" ], identifier, ":", type, ";"`. No new
  mechanism is introduced; the existing `mutable` flag that already flows
  `VarBinding` -> `ResolvedItem::Value { mutable }` -> `resolve_place_root` ->
  `require_mutable_place` is simply given the value the source wrote.

  **`mut` is an internal-mutation permission only, never a constancy claim.** The absence
  of `mut` does **not** mean the symbol's value is stable: a shared library, another
  object, or another thread may change it at any time. All a non-`mut` foreign binding
  says is that *this* Omega code does not write it. The mechanism that guarantees this is
  structural and must be preserved: **mutability stops at the analyzer.** It is not
  carried into `CheckedForeignBinding`, `MirForeignBinding`, `catalog::GlobalDecl`, or
  LLVM. Foreign globals are emitted through `declare_global` in
  `compiler/omega-codegen/src/llvm/item.rs`, which never calls `set_constant`, and a
  `CheckedForeignBinding` has no `initial_value` field to fold from. Both facts already
  hold; the job here is to *not* break them.

- **Rejected alternatives:**
  - *`mut foreign name : T;`* — puts the modifier before the item keyword, unlike every
    other binding in the grammar, and reads as if `foreign` itself were mutable.
  - *Plumbing `mutable` into MIR/codegen (e.g. LLVM `constant` for non-`mut`)* — directly
    violates the rule above. Non-`mut` is not `constant`.
  - *An `UnnecessaryMut` warning on a foreign binding* — unsound. `warn_unused_bindings`
    only sees one lexical scope; nothing tracks writes to a global from another module or
    another separately compiled package, so "declared `mut`, never written" is not
    knowable here. Do not extend that warning to globals or foreign bindings.
  - *Treating `foreign` as implying `volatile`* — out of scope and not what `mut` means.
    Omega has no `volatile` concept; asynchronous external modification is the atomics
    story (`docs/language/atomics.md`), unchanged by this work.

## Technical Details

- **Initial context boundary:** `compiler/omega-parser` (`parser/item/foreign.rs`,
  `ast/item.rs`, `diagnostics.rs`), `compiler/omega-hir` (`hir.rs`, `lower/item.rs`),
  `compiler/omega-analyzer` (`analysis/items/mod.rs`, `error/kind.rs`, `error/render.rs`),
  `compiler/omega-driver/src/items/resolution.rs`, plus
  `docs/language/foreign-function-interface.md`, `docs/language/bindings-and-mutability.md`,
  `docs/language/grammar.md`. MIR and codegen stay **closed** — see the direction above.

- **Affected files/symbols:**
  - `omega-parser/src/ast/item.rs` — `ForeignBindingItem` gains `mut_span: Option<Span>`.
  - `omega-parser/src/parser/item/foreign.rs` — `parse_foreign_item` and
    `parse_foreign_block_entry` consume a contextual `mut` before the identifier;
    `parse_foreign_binding_tail` records its span; a `mut` that turns out to precede a
    function form is diagnosed.
  - `omega-parser/src/diagnostics.rs` — new `ParseErrorKind::ForeignMutOnFunction`
    (enum variant + `to_diagnostic` arm), alongside the existing
    `ForeignConventionOnBinding` / `NestedForeignBlock`.
  - `omega-hir/src/hir.rs` — `HirForeignBinding` gains `mut_span: Option<Span>`.
  - `omega-hir/src/lower/item.rs` — `lower_foreign_binding` copies it.
  - `omega-analyzer/src/analysis/items/mod.rs` — `analyze_foreign_binding` rejects `mut`
    on a function-typed binding and passes the real flag to `declare_binding`
    (currently the literal `false` at the `DeclarationPolicy::Unique` call).
  - `omega-analyzer/src/error/kind.rs` + `error/render.rs` — new
    `AnalysisErrorKind::ForeignMutFunctionBinding` (enum variant, `Display` arm, render
    arm), following `NotMutableBinding` as the shape precedent.
  - `omega-driver/src/items/resolution.rs` — the `HirItem::ForeignBinding` arm currently
    builds `ResolvedItem::Value { ..., mutable: false }`; it must report the declared
    mutability. This is the only path a reference from another module or package uses.

- **Interfaces/invariants:**
  - **Single source of truth for mutability.** Carry only `mut_span: Option<Span>` in the
    AST/HIR; mutability *is* `mut_span.is_some()`. Do **not** add a parallel
    `mutable: bool` — that would be redundant state (ARCHITECTURE.md invariant 1) in a
    way `DeclarationStmt.mutable` is not, because `mut` there has no separate span to
    point a diagnostic at. Expose a small `is_mutable()` accessor if call sites read
    better for it.
  - `CheckedForeignBinding`, `MirForeignBinding`, `catalog::GlobalDecl`, and the LLVM
    global emission path are **unchanged**. Adding a `mutable` field to any of them is
    out of scope and contradicts the direction.
  - A foreign binding is still never readable at compile time: `comp_eval::read_root`
    rejects `Storage::Global` with `NonCompGlobalRead`, and that must keep holding for
    `mut` and non-`mut` alike.
  - `foreign` defaults (`SymbolPolicy::foreign()`, `mangle = disabled`, export behavior)
    are untouched by `mut`.
  - `mut` is contextual. `foreign mut : i32;` must still parse as a binding *named* `mut`,
    so the modifier is recognized only when the next token is an identifier — the same
    lookahead discipline `binding_modifiers_follow` in `parser/mod.rs` already uses.

- **Out of scope:** `volatile`/atomic semantics; changing non-`mut` foreign semantics in
  any way; mutability on foreign *functions* of any shape (declaration, definition with a
  body, direct or block entry); mutability on gap/glue-synthesized foreign bindings (those
  are always function-typed); the unrelated `expected.compiler.stderr` revert.

- **Risks/open questions:** none blocking. If the function-typed-binding rejection turns
  out to collide with a real runtime/`plat` declaration in-tree, stop and escalate rather
  than weakening the rule — `grep -rn "^foreign \|foreign .*:" runtime/` currently shows
  only `foreign _omg_main : () => void;` in two `plat` fragments, both non-`mut`.

## Implementation Plan

1. **Parser AST + grammar.** Add `mut_span: Option<Span>` to `ForeignBindingItem`. In
   `parse_foreign_item`, after `parse_optional_convention` and before `p.expect_ident()`,
   consume a contextual `mut` only when it is followed by an identifier token; thread the
   consumed span into `parse_foreign_binding_tail`. Do the same in
   `parse_foreign_block_entry`, after visibility and after the nested-`foreign` check.

2. **Reject `mut` on the function forms in the parser.** When a `mut` was consumed but the
   token after the identifier begins a function form (`(` or `<`), report the new
   `ParseErrorKind::ForeignMutOnFunction` at the `mut` span and then keep parsing the item
   as a foreign function, so the rest of the file still yields useful diagnostics — the
   same error-then-continue recovery `ForeignConventionOnBinding` already uses. Message
   intent: mutability applies to a foreign *binding*'s storage; a foreign function
   declares a code symbol, which nothing writes.

3. **HIR.** Add `mut_span: Option<Span>` to `HirForeignBinding` and copy it in
   `lower_foreign_binding`. No desugaring or normalization — this is a recorded fact.

4. **Analyzer.** In `analyze_foreign_binding`, after `resolve_type_or_error`: if the
   binding is `mut` and the resolved type is `ResolvedType::Function(_)`, report
   `AnalysisErrorKind::ForeignMutFunctionBinding` labeled at `mut_span` and return `None`.
   Otherwise pass `binding.mut_span.is_some()` as the `mutable` argument to
   `declare_binding` in place of the current `false`. Message intent: the binding names an
   external function symbol, not storage; write the convention/type instead, or bind a
   `*mut` if the intent was a mutable function *pointer variable*.

5. **Driver.** In `items/resolution.rs`, the `HirItem::ForeignBinding` arm must build
   `ResolvedItem::Value { mutable: binding.mut_span.is_some(), .. }`. Without this step
   the feature works nowhere except inside the declaring item's own analyzer scope.

6. **Docs.**
   - `docs/language/grammar.md`: `foreign-binding` becomes
     `[ visibility ], "foreign", [ "mut" ], identifier, ":", type, ";" ;` and
     `foreign-block-entry` gains `[ "mut" ]` before `identifier`. Extend the prose note
     after the block to state that `mut` is only valid on the `":", type` form, and that
     `mut` before an identifier followed by `(`/`<` is rejected.
   - `docs/language/foreign-function-interface.md`, "Foreign bindings": document
     `foreign mut name : Type;`, the C-shared-counter example, and — prominently — that
     omitting `mut` is *not* a claim that the value is constant. State that Omega derives
     no immutability optimization from a non-`mut` foreign binding, since the symbol may
     belong to a shared library or another object that writes it. Add the rule that `mut`
     is rejected on every foreign function form, including a function-typed binding.
   - `docs/language/bindings-and-mutability.md`, "What may be mutable": replace
     "A `foreign` binding is an immutable symbol binding." with the `foreign mut` rule and
     a cross-reference to the FFI chapter for the non-constancy caveat.
   - `docs/guide/quick-reference.md`: add `foreign mut` to the `@symbol`/foreign example
     block near line 484 so the writing-Omega entry point shows the spelling.
   - Nothing in `docs/architecture/` or `docs/issues/` needs to change: no architecture doc
     describes foreign-binding fields, and this closes no tracked issue.

## Testing

- **Parser (`compiler/omega-parser/tests/foreign.rs`) — component contracts:**
  - `foreign mut errno : i32;` yields `Item::ForeignBinding` with `mut_span.is_some()`.
  - `foreign errno : i32;` still yields `mut_span.is_none()` (extend
    `foreign_binding_parses`).
  - `foreign mut : i32;` yields a binding **named** `mut` with `mut_span.is_none()` —
    the contextual-keyword guarantee.
  - `foreign(c) { shared mut errno : i32; f(a: i32) => void; }` yields a
    `ForeignBlockEntry::Binding` with `mut_span.is_some()` and an unaffected function entry.
  - `foreign mut f(a: i32) => void;` and a block entry `mut f(a: i32) => void;` both report
    `ParseErrorKind::ForeignMutOnFunction`, following
    `foreign_convention_directly_on_binding_is_rejected` as the assertion shape.

- **Driver (`compiler/omega-driver/tests/diagnostics.rs`) — negative semantic case:**
  `foreign mut fp : (i32) => void;` is rejected with
  `AnalysisErrorKind::ForeignMutFunctionBinding`, using the existing `TestPackage` /
  `expect_errors` helpers. Assert the error *kind*, not rendered text.

- **Language conformance — `tests/t19_foreign_function_interface/`:** extend the existing
  case rather than adding a new package; it already owns the `native.c` + Omega FFI
  boundary and a `helper.omg` submodule.
  - Add to `native.c`: a non-static `int t19_counter;` and an
    `int t19_read_counter(void)` accessor so the C side's own view of the symbol is
    observable.
  - In the root module: `shared foreign mut t19_counter : i32;` and
    `shared foreign(c) t19_read_counter() => i32;`.
  - In `main`: plain assignment, a compound assignment, and `&mut t19_counter` written
    through a `*mut i32`, then print `t19_read_counter()` — proving the write lands on the
    external symbol, not on a local copy.
  - In `helper.omg`: write the same binding from another module via `import root::...`,
    exercising the `ResolvedItem::Value { mutable }` resolver path (step 5) that a
    same-module reference alone would not cover.
  - Update `tests/t19_foreign_function_interface/expected.stdout` accordingly.

- **Specification trace:** the conformance case proves the new "Foreign bindings" rule in
  `docs/language/foreign-function-interface.md` and the revised
  `docs/language/bindings-and-mutability.md` "What may be mutable" entry.

- **Negative/diagnostic cases:** add a `foreign mut` entry to
  `tests/t19b_foreign_abi_errors/` **only if** the resulting diagnostic is the rendered
  form worth pinning; the parser and driver tests above are the primary coverage, and
  `t19b` exists for ABI rejections specifically. Prefer not to widen it. What *must* be
  covered end-to-end is the positive direction plus the two Rust-level rejections. If a
  rendered-diagnostic case is added, it must pin the full expected `stderr`, since "it
  failed" is not evidence.

- **Regression coverage:** `cargo test -p omega-parser -p omega-hir -p omega-analyzer -p
  omega-driver` (the `mut_span` field addition touches every `ForeignBindingItem` /
  `HirForeignBinding` construction site, including `omega-hir/src/lower/tests.rs`).

- **Commands/target coverage:** `./bin/test-runner t19_foreign_function_interface` once
  `omgc` and the runtime objects are built, then `just test-all` as the full gate. No
  freestanding/backend-specific verification is needed: nothing in this change reaches MIR
  or codegen.
