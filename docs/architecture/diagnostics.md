# Diagnostics architecture

Omega keeps diagnostics structured until the CLI rendering boundary.

```text
lexer/parser/macro/analyzer/driver
        |
        | structured error/warning values + spans
        v
omega-driver accumulation / CompileError conversion
        |
        v
omgc
        |
        | Diagnostic + the driver's SourceRegistry
        v
omega-diagnostics::Renderer
        v
terminal text
```

## `omega-diagnostics` ownership

The crate is intentionally language-agnostic. It owns:

- `Span` — source byte range;
- `SourceId` / `SourceRegistry` — opaque compilation-local source identity and
  the retained text it addresses;
- `SourceSpan` — a `SourceId` paired with a `Span`, for any location that may
  outlive the module it was produced in;
- `SourceFile` — retained source text + line/column mapping;
- `Diagnostic` — severity/message/labels/footers;
- label styles;
- terminal renderer;
- generic syntax-highlighting interface.

It does not know Omega's AST, types, modules, or semantic error variants.

## Spans

A `Span` is a byte-offset range in one source file and carries no file identity of its own. Inside one module that is enough: the driver stamps the owning source at the rendering boundary. A location that crosses modules — a previous declaration, a conflicting conformance, a glue block, a compile-time call trace, a macro definition — must be a `SourceSpan` instead, so the renderer can never read one file's offsets against another's.

Frontend/HIR structures preserve specific spans for diagnosable constructs rather than relying on whole-parent spans.

## Retained source

`omega-driver::ModuleStore` owns the `SourceRegistry` and assigns a `SourceId` as soon as a physical file is read—even if parsing later fails. This ensures parse errors can still render snippets from the source that failed to produce HIR, and it is what lets any later phase name a file it does not otherwise own. Parser, HIR, and analyzer may carry source ids and `SourceSpan`s; none of them owns source text.

The source is kept for the compilation lifetime so later semantic errors can render after parsing/analyzing have long finished.

## Stage-specific findings

### Parser

`omega-parser::diagnostics` exposes structured `ParseError` / `ParseErrorKind` with spans/token descriptions.

### Macro expansion

`MacroError` pairs a `MacroErrorKind` with the sites the failure is actionable at: the macro's declaration (and the module that declared it) and the invocation being expanded. The driver resolves the declaring module to a `SourceId`, so a macro failure renders as a labeled diagnostic across both files rather than a headline string.

### Analyzer

`AnalysisError` / `AnalysisWarning` are typed semantic variants. Rendering conversion happens separately, preserving exact source facts and enabling targeted labels/suggestions.

### Driver

`CompileError` is the package/orchestration-level envelope for parse, macro, resolve, analysis, module-root, gap/glue, and other compilation-wide failures.

## Analyzer finding flow

One temporary `Analyzer` accumulates its own errors/warnings. `Analyzer::finish` returns them to the driver, which buckets findings by module.

`Analyzer::error`/`Analyzer::warn` are the single policy point for attribution. They stamp the module's `SourceId` on the finding, and the `_from` variants additionally resolve `(current module, span, origin)` to the honest authored site: macro-authored syntax is reported at the macro declaration with the invocation chain as secondary context, while caller-substituted syntax stays at the caller.

This has two benefits:

- semantic helper functions can record multiple useful errors instead of threading a fatal `Result` through every path;
- final rendering still occurs with the driver's source/module ownership intact.

## Warnings and suppression

Warnings remain semantic objects until final collection. `@suppress(...)` is resolved/activated in the analyzer scope where it applies; warnings that are genuinely suppressed should not later be “rediscovered” by renderer logic.

Whole-package warnings such as unused imports/dead code may be produced by driver sweeps after item/body analysis.

## Suggestions

“Did you mean” candidates are computed at the semantic layer that owns the candidate set:

- analyzer lexical context for locals/types;
- driver/module resolver for top-level names/import aliases;
- similarity helper for spelling distance.

The renderer should not perform semantic searches.

## Macro provenance

Macro-authored tokens carry expansion-origin information used during semantic resolution *and* diagnostic authorship. Expanded tokens keep call-site spans, because an expansion mixes template and substituted tokens and a single byte-offset space cannot describe both files; `Origin` is what identifies who wrote a construct.

`ExpansionState` retains each macro's declaration location plus every expansion's invoking module, call span, and parent expansion. `Analyzer::error`/`warn` resolve that into a source-qualified authored site, so a finding about macro-authored syntax points at the definition a developer can edit and shows the invocation as context. Findings about caller-substituted syntax are left where the caller wrote them.

Because one declaration is one actionable site, findings that repeat per invocation are collapsed at final collection rather than suppressed at the point they are produced.

## Recoverable frontend sweep

Parse, macro-expansion, and signature failures are *local* blockers, not compilation blockers. `omega-driver::Diagnostics` is the shared frontend sink: every phase records at the unit that failed and continues with the rest.

- A module whose prerequisites are unavailable is **poisoned**: work that depends on it is skipped rather than run against fabricated semantic data. Unrelated modules still produce their own diagnostics.
- Every failed item query retains why it failed. A dependent lookup may return the secondary "already failed" marker, but that marker is never recorded as a root error while its module has a finding of its own — the primary reason is always present.
- A finding identified by the same node, span, and rendered claim is recorded once, so a query a later phase re-runs does not duplicate its own diagnostic.
- Ordering is deterministic: module order over the compilation surface, each module's load failure ahead of its analysis errors.

Compilation then hits **one** frontend error barrier. Only past it do whole-program absence warnings (unused imports, dead code, unfilled gaps) run — a skipped module would make every such claim false — and only past it does MIR/codegen begin.

## Warning policy

Warnings are classified by whether their claim survives generic substitution. A redundancy that exists only because one concrete substitution made two types equal (`NoOpCast`, `AlwaysTrueFalseComparison`) is not a claim about the written source, so it is not emitted from a concrete instantiation: instructing a generic author to delete code that another instantiation needs is worse than a conservative missed warning. Everything else describes the declaration itself.

Findings created repeatedly from one source construct — once per generic instantiation, or once per macro invocation — are collapsed at final collection by their actionable site and rendered claim. A differing concrete payload keeps two findings distinct, because that payload is the useful fact.

## Rendering

`omega_diagnostics::Renderer` accepts a `Diagnostic` plus the `SourceRegistry` it resolves labels through. It renders one `--> file:line:col` section per source the diagnostic labels, the primary label's source first and the rest in first-label order. `render.rs` owns rendering orchestration/header/footer policy, while `render/snippet.rs` owns source-layout, underline, multi-line-label, and highlighting mechanics. It handles:

- source snippet/line numbering;
- primary/secondary labels;
- footers;
- syntax highlighting;
- color policy.

`omgc` decides whether stderr is a terminal and whether `NO_COLOR` disables color, then uses `OmegaHighlighter` from the parser as the language-specific highlighting implementation.

## CLI summary

`omgc` renders each compile diagnostic and then emits a final count summary when compilation fails. Backend/codegen errors that currently return plain strings are CLI/toolchain errors/internal failures rather than structured source diagnostics; moving a source-validity error earlier is preferable to adding backend-specific source rendering.

## Diagnostic design rules

1. Preserve structured variants until rendering.
2. Anchor against the smallest honest source span available.
3. Compute semantic suggestions where the semantic candidate set is known.
4. Do not make backend error strings the normal implementation of language validation.
5. Keep renderer language-agnostic; inject highlighting through the trait.
6. Do not duplicate source ownership in every compiler crate—module/source association belongs to the driver.

## Diagnostic representation and renderer invariants

The following details used to be repeated in Rust API doc comments and are centralized here instead.

- A `Diagnostic` is presentation-independent structured data: severity, headline, ordered labels, and ordered note/help footers. The first primary label determines the `--> file:line:column` header; if there is no primary label, the first label is used.
- A label names its own source, defaulting to the diagnostic's when it does not. Multi-file diagnostics are valid and normal; the renderer groups labels by source so one file's byte offsets are never interpreted against another's. A label whose source is unknown to the registry is dropped rather than rendered against the wrong file.
- `SourceFile` precomputes line-start byte offsets. Public positions are 1-based. Display columns count Unicode scalar values with tabs expanded using the same width as snippet rendering; they are not grapheme-cluster coordinates. Offsets at or just past EOF clamp, and offsets inside a multi-byte scalar back up to a valid character boundary, so malformed/recovery spans do not panic diagnostic rendering.
- `Span` is only a byte range and intentionally carries no file identity; `SourceSpan` adds it. File/module ownership remains with the driver, which is the only owner of retained source text and of source-id assignment.
- Syntax highlighting is injected through the language-agnostic highlighter interface. Highlight spans must be sorted and non-overlapping, and highlighting must tolerate lexically broken source; an unclassifiable region is simply rendered without a syntax class.
- Multi-line labels reserve a fixed continuation-bar area so source columns do not shift between ordinary and continued lines. Very large labeled ranges elide their middle rather than printing arbitrarily large snippets.
- Color policy belongs at the renderer/CLI boundary. Semantic/compiler crates should not embed terminal escape sequences in diagnostic messages.
