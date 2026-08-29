# Diagnostics and Error-Recovery Overhaul

## Task Description
- **Deliverable:** Refactor Omega's frontend diagnostic path so one recoverable failure does not hide unrelated errors; diagnostics retain honest source/provenance information across modules, macros, and generic instantiations; type names are complete enough to distinguish the values being compared; and warnings are emitted only when their claim is valid in the source context that the developer can act on.
- **Purpose:** Fix the recurring classes of diagnostic failure now visible across the compiler: phase-wide early aborts, false-positive/context-insensitive warnings, incomplete type rendering, wrong macro/cross-file blame sites, and secondary/rootless errors that lose the real cause. The goal is a durable diagnostic model rather than one-off exceptions for the examples that exposed it.
- **Chosen direction:**
  - Keep `Span` as the parser/HIR byte-range coordinate used inside one expanded module, but add source identity to *diagnostic* locations (`SourceId` + `Span`) so a `Diagnostic` can safely label more than one file. Do not make the parser compose byte ranges from different physical source files.
  - Extend macro provenance rather than continuing to overload call-site spans. Expanded syntax may keep call-site `Span`s for parsing/range composition and builtin `file$`/`line$`/`column$` semantics, while its `Origin` identifies who authored the syntax. Record enough definition/invocation location in `ExpansionState` to map a macro-authored finding back to the macro definition and optionally show the expansion site.
  - Treat parse/load/signature failures as local/query blockers, not automatically as compilation blockers. Continue through independent modules/items and stop only work whose prerequisites are unavailable. Backend/MIR/codegen still run only after the complete frontend diagnostic sweep is error-free.
  - Preserve the real cause of failed module/item queries. `ItemFailed`/load-failed-style errors remain secondary “already failed” states only; they must never become the sole visible diagnostic.
  - Add an explicit analyzer/lint context for concrete generic instantiations. Source-redundancy lints such as `NoOpCast` must not be inferred merely because substitution made two types equal. Declaration-stable warnings are deduplicated across instantiations; genuinely concrete/instantiation-specific diagnostics may remain concrete when their message is useful and honest.
  - Make resolved generic arguments part of diagnostic type formatting centrally; add a qualified fallback when two unequal types would still print identically with short names.
- **Rejected alternatives:**
  - Do not patch each warning with macro/generic special cases. That repeats the same provenance/context bug in every new lint.
  - Do not make all macro-generated tokens use physical definition-file byte offsets as their ordinary `Span`; an expanded expression can mix template and metavariable tokens from different files, so ordinary span-covering would become ill-defined.
  - Do not “recover” by continuing analysis with fabricated types/items. Failed prerequisites are poisoned/skipped; unrelated work continues. This avoids cascades and hidden compiler behavior.
  - Do not solve cross-file relationships by emitting ad-hoc duplicate diagnostics in each file. Make cross-file labels representable once in `omega-diagnostics`.
  - Stable error codes, machine-applicable edits, terminal grapheme-width work, and unrelated language-policy changes are not part of this task.

## Technical Details
- **Initial context boundary:**
  - `compiler/omega-diagnostics/` and `docs/architecture/diagnostics.md` for diagnostic data/source ownership/rendering.
  - `compiler/omega-driver/src/{compile,diagnostics,modules,error,items,resolver}.rs` for orchestration, query failure, module loading, and final finding collection.
  - `compiler/omega-analyzer/src/{analysis,error,resolved_type}.rs` for warning/error construction, generic-instantiation context, and type rendering.
  - `compiler/omega-parser/src/{macros,parser/recovery,ast}.rs`, `compiler/omega-hir/`, and `docs/architecture/parsing-and-hir.md` only for the provenance/location facts required by diagnostics.
  - Relevant debt/known behavior: `docs/issues/{design-debt,known-issues,compiler-limitations,language-limitations}.md`; testing rules in `docs/architecture/testing-and-validation.md`.
- **Affected files/symbols:**
  - `omega_diagnostics::{Span, SourceFile, Diagnostic, Label, Renderer}`: introduce compilation-local source identity and source-qualified label locations; render labels grouped by source with the primary source first.
  - `omega-driver::ModuleStore`: own the source registry (`module -> SourceId`, `SourceId -> SourceFile`) and keep failed-module causes available instead of consuming them through `take_failure`.
  - `omega-driver::Diagnostics`: broaden from analyzer-only buckets into the frontend finding sink needed by recoverable orchestration; retain deterministic module/source order and avoid duplicate previously-reported failures.
  - `Driver::{compile, local_module_paths, collect_extern_signatures, collect_signatures, check_bodies}`: replace phase-wide `?`/`fatal` exits for recoverable module/item failures with per-unit recording/continuation. The current end of `collect_signatures` must no longer abort all body checking merely because some signature/import diagnostics were accumulated.
  - `ItemQueryState` / `SpecQueryState` and the item-resolution path in `compiler/omega-driver/src/items/{mod,resolution}.rs`: failed states retain/identify their primary cause; a dependent lookup must not manufacture a rootless `ItemFailed`.
  - `CompileError::{Parse, MacroExpansion, Resolve, Analysis}` and CLI rendering in `compiler/omgc/src/app.rs`: final rendering no longer selects one ambient `SourceFile` for an entire diagnostic; macro errors become structured/located rather than headline-only.
  - `omega_parser::macros::{ExpansionState, ExpansionOrigin}` and `MacroDefinitionStmt`: retain the macro definition location and each expansion's invocation location. Propagate a syntax-owner `Origin` through general AST/HIR expression/statement/item nodes (not only the currently special-cased paths/declarations/member names) so downstream diagnostics can distinguish macro-authored syntax from substituted caller syntax.
  - `omega_analyzer::{AnalysisSite, AnalysisError, AnalysisWarning}` and location-bearing fields in `AnalysisErrorKind`: carry source-qualified diagnostic sites. `Analyzer::error`/`warn` should be the shared policy point that resolves the current module + `Origin` to the honest authored site and expansion trace.
  - `Analyzer::new_in` / driver `with_analyzer*`: pass an explicit analysis mode describing an ordinary declaration/body versus a concrete generic instantiation. Use that mode in warning policy and warning deduplication.
  - `analysis/exprs/operators.rs`: `NoOpCast` (and the same class of resolution-coincidence lint, notably `AlwaysTrueFalseComparison`) must not fire only because a concrete generic substitution made the statement true. Preserve existing non-generic warnings.
  - `resolved_type.rs` and `error/render.rs`: print type arguments for struct/union/enum/spec types and qualify otherwise-identical unequal type names when necessary.
  - `parser/recovery.rs`: replace “any `Ident` is a boundary” recovery with grammar-aware lookahead shared with item/statement dispatch where practical, and guarantee forward progress after a parse error.
  - Existing cross-file/mislocated cases to migrate onto the new location model include `MultipleGluesForGap`, duplicate/ambiguous conformances and conformance cycles, compile-time-evaluation traces, macro expansion/definition errors, and generic-instantiation-only declaration failures such as `ZeroSizedAggregate`.
- **Interfaces/invariants:**
  - `omega-diagnostics` remains language-agnostic; `SourceId` is opaque and it never learns Omega module/path semantics.
  - The driver remains the owner of retained source text and source-id assignment. Parser/HIR/analyzer may carry source ids/sites but do not own source files.
  - Ordinary parser/HIR `Span` continues to describe the expanded module's coordinate space. Macro-generated syntax keeps call-site spans where required for parsing and builtin location macros; `Origin`/expansion metadata is what changes diagnostic authorship.
  - A diagnostic label always names its own source. Multi-file diagnostics are valid and renderer grouping must never interpret one file's byte offsets against another file.
  - Caller-substituted macro syntax keeps caller provenance. Macro-authored syntax resolves/diagnoses at the macro definition, with the invocation available as expansion context. Do not globally relocate every diagnostic produced during a macro invocation.
  - Recovery never invents a valid semantic value. A failed module has no HIR-dependent work; a failed item signature has no body checking; a body may continue past a failed dependency only where the analyzer already has a sound recovery path.
  - Every “already failed”/poisoned query state has a retained primary diagnostic. A secondary failure may be suppressed to avoid cascades, but a primary cause may never disappear.
  - Error collection order is deterministic. Preserve source/module order and stable within-source discovery order so exact stderr tests are reliable.
  - Warnings that assert source redundancy must be true independent of concrete generic substitution. Prefer a conservative missed warning over instructing a generic author to remove code that other instantiations require.
  - Warnings created repeatedly from the same generic/macro-authored source construct are reported once when their claim and actionable source are declaration-stable; concrete warnings remain distinct only when their concrete detail is itself actionable.
  - Frontend errors prevent MIR/codegen, but do not prevent independent frontend diagnostic collection. Whole-program “absence” warnings (`unused`, `unfilled_gap`, dead code) should only run once the frontend is otherwise clean so a failed/skipped module cannot create false warnings.
  - No runtime, ABI, freestanding, libc, or generated-code behavior changes are introduced by this diagnostics refactor.
- **Out of scope:**
  - Stable diagnostic/error codes and public documentation keyed by codes.
  - Machine-applicable replacement edits / IDE fix-it protocol.
  - Unicode grapheme/terminal-cell width improvements in snippet rendering.
  - Changing the language to accept/reject new programs solely to improve a diagnostic. In particular, the known non-integer-index codegen failure needs a separate language-policy decision if the normative index domain is not already specified; do not silently choose that rule here.
  - General incremental/parallel compilation or splitting the large `ModuleResolver` capability surface.
  - Redesigning generic semantics to carry symbolic generic types through codegen solely for lints.
- **Risks/open questions:**
  - If propagating a general syntax-owner `Origin` through AST/HIR exposes a construct whose ownership cannot be chosen deterministically (for example a node synthesized from both template and substituted tokens with no clear syntax introducer), stop and document the exact shape before inventing a location rule. The default should be the token that owns the syntactic construct, not “always caller” or “always macro”.
  - If body checking currently assumes every indexed item is resolved in a way that cannot be made conditional without fabricating semantic data, skip that body/item and continue others; do not weaken an invariant merely to increase the error count.
  - If a warning cannot be classified confidently as declaration-stable versus concrete-instantiation-specific, keep it conservative and add a focused test before expanding its scope.

## Implementation Plan
1. **Make source-qualified diagnostics a first-class renderer contract.**
   - In `compiler/omega-diagnostics`, add opaque `SourceId` and a source-qualified location (`SourceId + Span`), migrate `Label`/`Diagnostic::with_label`/secondary-label APIs, and change `Renderer` to resolve source text per label rather than receiving one ambient `SourceFile`.
   - Render the primary source section first and any additional source sections in first-label order; keep existing same-file output stable where possible.
   - In `ModuleStore`, assign/store source ids as soon as source text is retained (including parse failures), expose lookup by id, and update `omgc` to render through the driver's source registry.
   - Convert same-file parse/analyzer/driver diagnostics mechanically first so the tree builds before adding cross-file behavior.

2. **Separate diagnostic authorship from expanded byte ranges for macros.**
   - Give `MacroDefinitionStmt` a retained declaration/body location and extend `ExpansionOrigin` to retain definition location plus invocation module/span (and enough parent information for nested expansion traces).
   - Keep expansion tokens' ordinary `Span` behavior compatible with current parser/builtin semantics, but propagate each construct's syntax-owner `Origin` through the general AST/HIR node wrappers and lowering path.
   - Extend `AnalysisSite`/checked expression locations as needed so `Analyzer::error` and `Analyzer::warn` can resolve `(current source, span, origin)` to a source-qualified primary site. Caller substitutions remain caller-owned; macro-authored syntax becomes definition-owned. Add the invocation as a secondary expansion label/note when it materially helps.
   - Remove the ad-hoc `origin.0.is_some()` suppression in `warn_unused_bindings`; once attribution is honest, macro-authored unused/mut warnings can be diagnosed at the definition like other macro-authored warnings.
   - Make `MacroError` variants carry the relevant definition/invocation site and convert `CompileError::MacroExpansion` to a labeled diagnostic instead of `Diagnostic::error(error.to_string())`.

3. **Migrate secondary/cross-file locations and close the known misleading-site cases.**
   - Change location-bearing `AnalysisErrorKind` payloads from unqualified `Span` where needed to source-qualified locations, including previous declarations/conformances, cycle/compile-time traces, and any driver-created relationship diagnostic that may cross modules.
   - Update conformance registration/solver diagnostics so the “other conformance” label names its actual source, not the current module's byte space.
   - Change `sweep_gaps`/`MultipleGluesForGap` to label each conflicting glue block directly instead of exposing `<module>#<HirId>` implementation names.
   - Track the triggering concrete use/instantiation site for errors that exist only for one generic instantiation (the known `ZeroSizedAggregate` case is the regression target); retain the generic declaration as secondary context rather than blaming apparently healthy generic source alone.

4. **Turn driver compilation into a recoverable frontend sweep.**
   - Refactor `ModuleStore` load failure handling so the original `Parse`/`MacroExpansion`/compile cause is retained and reportable once; remove destructive `take_failure` semantics from recovery control flow.
   - Expand `omega-driver::Diagnostics` into the shared frontend sink for parse/macro/resolve/analysis errors. Record errors immediately at their owning unit, but do not convert “has any errors” into an early phase return.
   - `local_module_paths`: enumerate all local modules, record every independently discoverable/read/parse failure, and return the successfully parsed subset for later work. An empty *discoverable* package remains a true package-level blocker.
   - `collect_extern_signatures`: parse/index each extern module independently; a failed extern module is poisoned/skipped while other extern modules continue.
   - `collect_signatures`: catch `ensure_module_indexed`, normalization, generic-template, item, and overload-signature failures per module/item; record the real error and continue. Remove the current `drain_errors(local) -> Err` barrier at the end of this phase.
   - `check_bodies`: visit only items whose signatures/query prerequisites resolved. A failed item is skipped; other items/modules still produce diagnostics. Replace phase-wide `map_err(fatal)?` exits with per-unit recovery.
   - Run remaining relationship/error-producing frontend sweeps over successfully registered data, then perform one final frontend error barrier. Only if that barrier is clean should dead-code/unused/unfilled-gap warnings and MIR/codegen proceed.

5. **Make failed query state preserve its primary cause and suppress only true cascades.**
   - Replace bare `ItemQueryState::Failed` / `SpecQueryState::Failed` with failure state that distinguishes an underlying reportable cause from a failure already recorded by analyzer/driver diagnostics.
   - When `ensure_item`/generic-bound/signature resolution first fails, retain the underlying cause. Later dependent lookups may return an “already failed” result, but the diagnostic sink must not emit it as a new root error.
   - Fix the existing generic/non-generic overload rootless-`ItemFailed` regression while applying this invariant; the final output must always contain the primary reason before/sans any secondary “cannot use because of its own error” message.
   - Add assertions/component tests around the invariant so a future query path cannot silently create a failed state without a retained primary.

6. **Improve parser recovery so syntax errors also benefit from aggregation.**
   - Rework `parser/recovery.rs` boundary checks to use grammar-aware lookahead rather than treating every identifier as an item/statement boundary. Reuse/extract the lightweight dispatch predicates from `parser/item.rs` and `parser/statement.rs` where possible so recovery does not develop a second grammar.
   - Ensure synchronization consumes input after an error unless already at a proven enclosing boundary, respects balanced delimiters/closing braces, and does not let one malformed member swallow the enclosing block.
   - Keep parser recovery conservative: recover to the next credible construct; do not synthesize AST nodes merely to increase the number of errors.

7. **Introduce explicit warning context for generic instantiations and centralized deduplication.**
   - Pass an analyzer mode from `with_analyzer*` describing ordinary analysis versus a concrete generic instantiation. Keep it diagnostic-only; do not change `ResolvedType` semantics/codegen to carry symbolic generics.
   - Classify warning kinds by reporting policy in one place. At minimum, post-resolution coincidence/redundancy warnings (`NoOpCast`, and analogous type-domain coincidence such as `AlwaysTrueFalseComparison`) must not be emitted merely from a concrete generic instantiation. The user's `<*T>...` instantiated as `T = u8` case must be warning-free while the ordinary concrete `<u32>a` identity cast still warns.
   - Deduplicate declaration-stable warnings generated once per generic instantiation or macro invocation by source site + warning identity. Do not collapse concrete warnings whose differing concrete payload is the useful fact.
   - Add macro pre-pass import-usage accounting so resolving/invoking an imported macro marks that import used before HIR dead-import analysis; remove the known spurious `unused import` warning for macro imports.

8. **Make diagnostic type text complete and disambiguating.**
   - Update `Display for ResolvedType` so struct/union/enum/spec instances include resolved type arguments recursively; preserve enum variant suffixes after the instantiated type name.
   - Add a diagnostic formatter/fallback in `error/render.rs` for pairs/lists of types: if two unequal types still have the same short rendering (for example same declaration name from different modules), qualify enough of the module path to make the distinction visible rather than printing `expected X, found X`.
   - Update all affected exact-message tests, especially `tests/t32b_try_operator_errors`, and add direct resolved-type formatting tests for nested generics/specs/enum variants.

9. **Update diagnostic architecture/debt documentation after behavior is proved.**
   - Rewrite the single-source-label and macro-call-site invariants in `docs/architecture/diagnostics.md` and `docs/architecture/parsing-and-hir.md`; document recoverable frontend poisoning, the final frontend error barrier, macro authored-site vs invocation-site behavior, and generic warning policy.
   - Remove or narrow resolved entries in `docs/issues/design-debt.md`, `known-issues.md`, `compiler-limitations.md`, and `language-limitations.md`: source-less spans/cross-file labels, `MultipleGluesForGap`, generic type arguments omitted from diagnostics, macro-authored unused locals, spurious macro-import unused warning, coarse parser recovery, rootless `ItemFailed`, and the generic-instantiation diagnostic-site case.
   - Do not delete unrelated diagnostics debt (error codes/fix-its, terminal width, etc.).

## Testing
- **New/changed cases:**
  - `compiler/omega-diagnostics/src/render/tests.rs`: same-file rendering remains stable; a diagnostic with primary/secondary labels in different `SourceId`s renders both correct file names/snippets and never indexes one source with the other's offsets.
  - `compiler/omega-parser/src/tests.rs`: two or more independent syntax errors in one file are all returned; malformed member recovery does not swallow the enclosing brace; identifier-heavy malformed input makes forward progress.
  - `compiler/omega-driver/tests/` (add a focused diagnostics/recovery integration file if existing topical files become awkward):
    - independent errors in separate local modules survive one module's bad import/parse/signature;
    - multiple independent errors after an import/signature error are reported in the same compile;
    - a failed signature skips only its own body while another valid-signature body is still analyzed;
    - dependent uses of a failed item do not replace the primary with rootless `ItemFailed` spam;
    - macro-authored unused-return/unused-local diagnostics point to the macro definition, while a diagnostic caused by a substituted caller expression stays at the caller; nested expansion includes useful invocation context;
    - cross-module duplicate/ambiguous conformance and multiple-glue diagnostics label all real source sites;
    - generic declaration-stable warnings are not duplicated per instantiation.
  - `compiler/omega-driver/tests/casts.rs`: concrete identity cast still yields `NoOpCast`; a cast whose equality exists only after substituting a generic parameter yields no `NoOpCast`; test at least two instantiations so one coincidental identity cannot make the generic source warning appear.
  - Macro import tests in `compiler/omega-driver/tests/{aliases,macro_hygiene}.rs`: an actually invoked imported macro is not reported unused; a genuinely unused macro import still warns.
  - `compiler/omega-analyzer` resolved-type/error-render tests: generic struct/union/enum/spec arguments render recursively; two same-named unequal types can be disambiguated.
  - Root `tests/t32b_try_operator_errors/expected.stderr`: replace `Result`/`Result` ambiguity with the actual instantiated types. Add one focused end-to-end negative diagnostics-recovery package only if needed to verify CLI ordering/multi-error rendering beyond component tests.
- **Specification trace:** No Omega acceptance/runtime semantics are intentionally changed. Existing invalid programs remain invalid and valid programs remain valid; this task changes how many independent frontend errors are surfaced, where findings point, and warning correctness. The cast tests must continue to respect the existing explicit-cast behavior in `docs/language/strings-casts-arrays-and-slices.md`; warning suppression for substitution-only redundancy is a diagnostic policy, not a new cast semantic.
- **Negative/diagnostic cases:** Exact assertions should prove (1) all independent errors are present, (2) cascaded/previously-reported failures are absent or secondary only, (3) file/line labels are from the correct physical source, (4) macro caller vs definition blame is correct, and (5) expected/found type strings cannot be identical when the resolved types differ.
- **Regression coverage:** Existing driver suites most likely to expose regressions are `casts`, `macro_hygiene`, `aliases`, `conform`, `hidden_visibility`, `try_operator`, and `shadowing`; parser and diagnostics crate tests cover recovery/rendering internals. Preserve warning suppression behavior (`@suppress`) and builtin macro location semantics (`file$`/`line$`/`column$`).
- **Commands/target coverage:** Run focused crate tests while landing each step, then `just test-all`. Run `bin/test-runner t32b_try_operator_errors` (and any new root diagnostic case) for exact CLI output. No extra hosted/freestanding/backend matrix is required because the work is frontend/diagnostic-only; one full existing conformance gate is sufficient to prove no code-generation/runtime behavior changed.
