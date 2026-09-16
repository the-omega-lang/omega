# Source-level annotations (`@[...]`)

## Task Description

- **Deliverable:** a new annotation placement — `@[name(args)]` written in a prologue before any
  top-level item of a `.omg` file — with source-scope semantics defined per annotation. Two
  annotations become source-level: `@[cond(...)]` and `@[suppress(...)]`. Everything else is
  rejected at source level with a specific diagnostic.

- **Purpose:** whole-file conditional compilation (the `runtime/plat/*` fragments are the driving
  case: a platform source and its submodules should disappear together under one flag) and
  whole-file warning suppression, without writing the same annotation on every declaration.

- **Chosen direction:**

  1. **Parsing.** `@` followed by `[` is a *source-level* annotation. `parse_source_module` consumes
     a prologue of them before the first item; `SourceModule` grows an `annotations` field. `@[`
     anywhere else (after the first item, in a statement, on a member, inside a macro body) is a
     parse error. The bracket is the only syntactic difference — the name/argument grammar is
     shared verbatim with node-level annotations.

  2. **Evaluation.** A new eager **module-selection prepass** in `omega-driver` runs at the top of
     `Driver::compile`, before `collect_extern_signatures` and `local_module_paths`. For every
     discovered source-bearing module in the local tree and every extern tree, in sorted module-path
     order (parents before children), it parses the file, resolves its source-annotation prologue,
     and — when `@[cond]` evaluates to **false** — removes that module *and its whole subtree* from
     `ModuleRoots`. A pruned module is then indistinguishable from a module that was never on disk:
     `locate` reports `UnknownModule`, `local_sources()` omits its file so no artifact slot exists,
     and no later phase needs a "disabled" concept. Rule 2 of the request (children of a disabled
     main file are disabled) therefore falls out of subtree pruning rather than a special case.

  3. **Only a successful `false` prunes.** If the file fails to parse, or its prologue is malformed,
     or its condition cannot be evaluated, the module stays in the tree and the existing per-module
     failure paths report it exactly as today. This keeps every diagnostic attached to a module that
     is still in the compilation's diagnostic surface, so the prepass itself never has to report
     anything.

  4. **Two-stage prologue resolution**, mirroring the existing node-level split (the parser already
     partitions `cond` out of an item's annotation list, and the driver decides conditions before
     anything else validates the rest):
     - *Stage 1 — existence:* find `@[cond]`; a second one is an error; evaluate the single
       condition through the existing `annotation_eval::evaluate`. False ⇒ prune, and **stop** —
       nothing else about the file is inspected, matching "once an outer condition is valid and
       false, the declaration is discarded without further inspection".
     - *Stage 2 — validation:* for a surviving module, validate the remaining source annotations
       (recognized name, source-level applicable, no duplicate, well-formed arguments) and record
       the resulting `SourceAnnotations` (currently just the `@[suppress]` name list) per module.

  5. **`@[suppress]` semantics.** Not "apply `@suppress` to every item". Defined directly as: *a
     warning reported against this module's source is dropped if its name appears in this file's
     source-level suppression list, unless the warning is not suppressible.* Every warning in the
     driver is already carried as `(ModulePath, AnalysisWarning)`, so this is one filter over the
     final warning list in `Driver::compile`, after `deduplicate_warnings`. It therefore covers the
     warnings the short-lived `Analyzer` raises **and** the whole-program sweeps that never go
     through `Analyzer::warn` (`report_unused_imports`, `sweep_dead_code`, gap sweeps) — which a
     per-item desugaring would cover only unevenly.

  6. **Source annotations never reach HIR.** The prepass reads the prologue off the raw AST; the
     filtered AST that `ensure_ast` publishes carries an empty `annotations` vector, mirroring the
     existing invariant that no consumer ever sees a non-empty `ItemNode::conditions`.

- **Applicability verdict** (this is the settled decision; do not re-litigate it during
  implementation):

  | Annotation | Source level | Why |
  |---|---|---|
  | `@cond` | **yes** | a file is a selectable unit of compilation; source scope has its own meaning (the module, and its children, do not exist) beyond per-item filtering |
  | `@suppress` | **yes** | warning scope is naturally a lexical region; "warnings reported against this file" is a real scope, distinct from any item's scope |
  | `@layout` | no | a file has no layout. A file-wide default would be exactly the forbidden "apply to every struct" desugaring, and would make a struct's layout depend on invisible distant syntax |
  | `@inline` | no | a file has no body to inline |
  | `@naked` | no | a file has no machine-level body |
  | `@symbol` | no | a file owns no symbol; `name`/`mangle` are meaningless or collide per file |

- **Rejected alternatives:**
  - *Source-level `@symbol(export)` as a file-wide default visibility.* This is coherent as a
    *default* rather than a desugaring, but it makes a declaration's binary visibility unreadable
    from the declaration, which is the hidden-behavior Omega rejects; `visibility.md` also keeps
    binary visibility a deliberate per-item decision. Not in scope.
  - *Deciding `@[cond]` lazily inside `ensure_ast`.* Module existence must already be settled when
    imports resolve, when the local module sweep enumerates the package, and when `local_sources()`
    decides native emission granularity. A lazy decision would force every one of those to learn
    about a "disabled" state; pruning the tree keeps existence a single fact in one place.
  - *Reusing `ItemNode::conditions` by attaching the source condition to every parsed item.*
    Explicitly forbidden by the request, and it could not express "the children do not exist".
  - *Seeding the source suppression list into `Analyzer::suppressed`.* Would be a second mechanism
    that still misses every driver-level sweep warning.

## Technical Details

- **Initial context boundary:**
  - `compiler/omega-parser/src/parser/item.rs`, `parser/item/annotations.rs`, `src/lib.rs`,
    `src/diagnostics.rs`, `src/macros.rs`;
  - `compiler/omega-analyzer/src/annotation_eval.rs`, `src/annotations.rs`;
  - `compiler/omega-driver/src/roots.rs`, `src/modules.rs`, `src/error.rs`,
    `src/compile/mod.rs`, `src/compile/output.rs`;
  - `docs/language/annotations-and-sizeof.md`, `docs/language/grammar.md`;
  - `docs/architecture/module-driver-and-linkage.md`.

  Codegen, MIR, mangling, and the analyzer's checking passes stay closed — no contract of theirs
  changes.

- **Affected files/symbols:**

  *Parser*
  - `SourceModule` (`omega-parser/src/lib.rs`): add `pub annotations: Vec<AnnotationNode>`.
    `SourceModule::parse` fills it.
  - `parse_source_module` (`parser/item.rs`): consume the `@[...]` prologue first, then the existing
    item loop. Reuse `parse_annotation` for the name/argument body; only the `[` / `]` framing is new.
  - `parse_item_annotations` / `parse_annotations` (`parser/item/annotations.rs`): on `@` + `[`,
    report the new error and consume the annotation so recovery makes progress. This is the single
    site that rejects a misplaced `@[...]` in item, statement, member, and macro-body positions.
  - `ParseErrorKind` + rendering (`src/diagnostics.rs`): one new variant, e.g.
    `SourceAnnotationNotInPrologue`, worded so it reads correctly both mid-file and inside a macro
    body ("source-level annotations must come before any top-level declaration"). Follow the shape
    of the neighbouring `ConditionNotAllowedHere`.
  - `macros::expand_with_origins` (`src/macros.rs:487`): carry `module.annotations` through to the
    returned `SourceModule` so the function stays total. It always receives an already-stripped tree.

  *Analyzer* — new `compiler/omega-analyzer/src/source_annotations.rs` (plus `source_annotations/tests.rs`),
  registered in `lib.rs`. It sits beside `annotation_eval.rs` and `compiler_definitions.rs`, which
  `ARCHITECTURE.md` already documents as the pure syntax-plus-configuration functions used before
  HIR exists.
  - `pub struct SourceAnnotations { pub suppress: Vec<Ident> }`;
  - `pub fn source_condition(&[AnnotationNode]) -> Result<Option<&AnnotationNode>, SourceAnnotationError>`
    (stage 1: locate `@[cond]`, reject a duplicate with a source-level message);
  - `pub fn resolve(&[AnnotationNode]) -> (SourceAnnotations, Vec<SourceAnnotationError>)`
    (stage 2: skips `cond`, rejects duplicates, unknown names, node-only names, bad arguments);
  - `pub struct SourceAnnotationError { span, origin, kind }` with a `Display` + `label()` in the
    style of `ConditionError`.
  - Condition *evaluation* reuses `annotation_eval::evaluate` unchanged.
  - To distinguish "unknown annotation" from "known but not source-level", export the recognized
    annotation-name set from one place — add a `pub const` list in `annotations.rs` and have the
    existing `match` in `annotations::resolve` and the new module both key off it, so the two cannot
    drift.

  *Driver*
  - `ModuleStore` (`modules.rs`): add a raw-AST cache and a
    `source_annotations: HashMap<ModulePath, SourceAnnotations>`.
  - Split `Driver::ensure_ast` into `ensure_raw_ast` (read file, register `SourceId`, parse, cache,
    record `LoadFailure::Parse`) and the existing item-condition filtering, which additionally
    strips `annotations` from the published `SourceModule`. Give `ensure_raw_ast` a failure
    short-circuit so the prepass and the later passes do not re-read a file that already failed.
  - New `compiler/omega-driver/src/selection.rs` with `Driver::select_modules(&mut self)`:
    the prepass described above. Deterministic order: sort module paths by segment.
  - `ModuleRoots::prune(&mut self, disabled: &[ModulePath])` in `roots.rs`: remove each disabled
    path and every descendant from `local_tree` / `extern_trees`, and **recompute `local_identity`**
    (the package root itself can be pruned, and `is_known_top_level` reads that field).
  - `CompileError` (`error.rs`): new `SourceAnnotation { module, error }` variant; extend
    `CompileError::module()` and the diagnostic rendering, following `CompileError::Condition`.
  - `CompileError`: new variant for "every local module was conditioned out" so the user does not
    get `EmptyPackage`'s misleading "this package declares no module" against a file that exists.
    Raise it from `select_modules` when pruning empties a previously non-empty local tree.
  - `Driver::compile` (`compile/mod.rs`): call `select_modules` first; after
    `deduplicate_warnings`, call a new `apply_source_suppressions(&mut warnings)` in
    `compile/output.rs` that drops `(module, warning)` pairs whose module suppresses
    `warning.kind.name()`.

- **Interfaces/invariants:**
  - `AnalysisWarningKind::is_suppressible()` must be honoured by the source-level filter.
    `unfilled_gap` is deliberately not suppressible and is emitted by the driver sweep, so a plain
    name match would otherwise reach it where node-level `@suppress` never can.
  - Nothing downstream of `ensure_ast` may observe a non-empty `SourceModule::annotations`, exactly
    as nothing may observe a non-empty `ItemNode::conditions`.
  - The prepass prunes only on a cleanly-evaluated `false`. Parse failure, malformed prologue, and
    condition-evaluation failure all leave the module present.
  - One configuration applies to every source the invocation reads, externs included — extern trees
    are pruned on the same rules as the local tree.
  - `CompiledProgram::sources` must keep deriving from `ModuleRoots` alone; pruning is what removes
    a disabled file's emission slot, and no separate filter should be added downstream.
  - Extern-module warnings are not reported today, so `@[suppress]` in an extern source is inert.
    That is expected, not a bug to work around.
  - **A pruned module and its descendants are not read at all** — settled, do not revisit. The
    annotated file itself is fully parsed (it has to be, to reach the prologue), so "disabled source
    must be well formed" still holds for every file the compilation reads, but it stops at the file
    boundary rather than covering the package. This narrows the `@cond` well-formedness rule to
    *files the compilation reads*, so `docs/language/` must state it explicitly instead of leaving
    it implicit.

- **Out of scope:** any new annotation; source-level `@layout`/`@inline`/`@naked`/`@symbol`;
  changing node-level annotation behavior; changing the condition grammar, operators, or definition
  namespaces; per-directory or package-level annotations.

- **Risks/open questions (escalate, do not decide alone):**
  - Pruning a `core` module (possible, since externs are pruned too) can remove primitive or
    conformance registrations and produce distant secondary errors. Do not add a guard; just confirm
    the failure is a normal resolution error rather than a panic.

## Implementation Plan

1. **Parser: prologue grammar.** Add `annotations` to `SourceModule`; parse the `@[...]` prologue in
   `parse_source_module`; carry the field through `macros::expand_with_origins`. Update existing
   `SourceModule { .. }` construction sites (`macros.rs:487`, `modules.rs:231`).
2. **Parser: rejection everywhere else.** Add the `ParseErrorKind` variant and its rendering, and
   reject `@` + `[` in `parse_item_annotations` / `parse_annotations`. Add parser tests
   (see Testing).
3. **Analyzer: `source_annotations.rs`.** Add the recognized-name constant to `annotations.rs` and
   make `annotations::resolve` read it. Implement stage 1 / stage 2 and their errors. Unit tests.
4. **Driver: raw-AST split.** Extract `ensure_raw_ast` from `ensure_ast`, add the raw cache and the
   parse-failure short-circuit, and strip `annotations` when publishing the filtered AST. No
   behavior change yet; the tree should build and the existing driver tests pass here.
5. **Driver: `ModuleRoots::prune`.** Subtree removal plus `local_identity` recomputation, with a
   unit test in `roots.rs`'s existing test module.
6. **Driver: the selection prepass.** `selection.rs` + `Driver::select_modules`, called first in
   `Driver::compile`. Add `CompileError::SourceAnnotation` and the fully-disabled-package error,
   with rendering and `CompileError::module()` coverage.
7. **Driver: source-level suppression.** Store `SourceAnnotations` per module; add
   `apply_source_suppressions` in `compile/output.rs` honouring `is_suppressible()`; call it after
   `deduplicate_warnings`.
8. **Docs.** `docs/language/annotations-and-sizeof.md` (new "Source-level annotations" section: the
   `@[...]` grammar, the prologue rule, the applicability table above with the reason each rejected
   annotation is rejected, `@[cond]`'s module/subtree semantics including the file-boundary
   narrowing recorded under Interfaces/invariants, `@[suppress]`'s "warnings reported against this file" scope,
   and the per-annotation multiplicity rule); `docs/language/grammar.md` (`source-annotation`
   production, `module = { source-annotation }, { [ annotation-list ], item } ;`, and update the
   note under the compilation-unit block); `docs/language/modules-and-imports.md` (a module whose
   source is conditioned out does not exist, and neither do its children);
   `docs/guide/quick-reference.md` (one short example);
   `docs/architecture/module-driver-and-linkage.md` (the prepass, its position before the two-phase
   local compilation, the "only a clean false prunes" invariant, and warning filtering by source
   suppression); `docs/architecture/parsing-and-hir.md` (`SourceModule::annotations` and that it is
   stripped before publication).
9. **Tests.** Add the crate-local and conformance cases below.

## Testing

- **New/changed cases:**

  *`omega-parser` (`tests/` — extend `tests/conditional_compilation.rs` or add
  `tests/source_annotations.rs`):* a prologue of several `@[...]` parses into
  `SourceModule::annotations` with the item list unaffected; bare `@[name]` and `@[name()]` both
  parse; a file containing only a prologue parses; `@[cond(...)]` after the first item is a parse
  error; `@[...]` before a `struct` field, before a statement, and inside a macro body are parse
  errors; a node-level `@cond` in the prologue position still attaches to the following item.

  *`omega-analyzer` (`src/source_annotations/tests.rs`):* duplicate `@[cond]`; duplicate
  `@[suppress]`; `@[inline]` / `@[layout]` / `@[naked]` / `@[symbol(export)]` each rejected as
  not source-level (distinct from unknown); `@[bogus]` unknown; `@[suppress(key = value)]` invalid
  arguments; `@[suppress(a, b)]` yields both names.

  *`omega-driver` (`tests/source_annotations.rs`):* a false `@[cond]` on a module's own file removes
  the module *and* its child module, and `import` of either reports `UnknownModule`; a true
  condition leaves both; `CompiledProgram::sources` omits the pruned file; a malformed `@[cond]`
  leaves the module present and surfaces `CompileError::SourceAnnotation`; a condition that cannot
  be evaluated surfaces the existing `CompileError::Condition` path; pruning every local module
  produces the fully-disabled-package error rather than `EmptyPackage`.

  *Root conformance (`tests/`, following the `t45*` conditional family):*
  - `t45i_source_conditional` — package with `thing/thing.omg` + `thing/child.omg`, a
    `compiler.definitions` file enabling the flag, `expected.stdout` proving both compile and run.
  - `t45j_source_conditional_children` — same package with the flag absent and an `import
    thing::child;`, `expected.stderr` proving the child module is unknown because its parent's
    source is conditioned out.
  - `t45k_source_suppress` — a file whose declarations would produce `unused_variable` and
    `unused_import`, with `@[suppress(unused_variable, unused_import)]`; `expected.stderr` proves
    neither warning is reported while an unrelated warning still is.
  - `t45l_source_annotation_after_item` — negative, parse diagnostic.
  - `t45m_source_annotation_not_source_level` — negative, `@[inline]` diagnostic.
  - `t45n_source_conditional_duplicate` — negative, duplicate `@[cond]` diagnostic.

- **Specification trace:** the new "Source-level annotations" section of
  `docs/language/annotations-and-sizeof.md` (prologue placement, applicability table, `@[cond]`
  module/subtree semantics, `@[suppress]` file scope, multiplicity) and the updated module-existence
  rule in `docs/language/modules-and-imports.md`.

- **Negative/diagnostic cases:** every negative case above must pin its exact `expected.stderr`;
  "it failed to compile" is not evidence. In particular `t45m` must show the *not source-level*
  wording rather than *unknown annotation*.

- **Regression coverage:** `t45`–`t45h` (node-level conditions must be unchanged), `t15`
  (annotations), `t12` / `t12b`–`t12e` (module resolution and its error wording), `t36_multi_object`
  (per-source emission granularity, which pruning now also feeds), and
  `compiler/omega-driver/tests/conditional_compilation.rs`.

- **Commands/target coverage:**
  `cargo test -p omega-parser -p omega-analyzer -p omega-driver` during steps 1–7;
  `just test-all` as the gate;
  `./bin/test-runner t45i_source_conditional t45j_source_conditional_children t45k_source_suppress
  t45l_source_annotation_after_item t45m_source_annotation_not_source_level
  t45n_source_conditional_duplicate` for the focused loop once artifacts are built.
  No backend- or freestanding-specific verification is needed: nothing below HIR changes.
