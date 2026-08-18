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
        | Diagnostic + optional SourceFile
        v
omega-diagnostics::Renderer
        v
terminal text
```

## `omega-diagnostics` ownership

The crate is intentionally language-agnostic. It owns:

- `Span` — source byte range;
- `SourceFile` — retained source text + line/column mapping;
- `Diagnostic` — severity/message/labels/footers;
- label styles;
- terminal renderer;
- generic syntax-highlighting interface.

It does not know Omega's AST, types, modules, or semantic error variants.

## Spans

A `Span` is a byte-offset range in one source file. Module/file association lives outside the span; the driver knows which module's retained `SourceFile` to pair with a finding.

Frontend/HIR structures preserve specific spans for diagnosable constructs rather than relying on whole-parent spans.

## Retained source

`omega-driver::ModuleStore` records a `SourceFile` as soon as a physical file is read—even if parsing later fails. This ensures parse errors can still render snippets from the source that failed to produce HIR.

The source is kept for the compilation lifetime so later semantic errors can render after parsing/analyzing have long finished.

## Stage-specific findings

### Parser

`omega-parser::diagnostics` exposes structured `ParseError` / `ParseErrorKind` with spans/token descriptions.

### Macro expansion

`MacroError` records expansion/definition/invocation failures. The driver retains enough module/source context to present them as compile errors.

### Analyzer

`AnalysisError` / `AnalysisWarning` are typed semantic variants. Rendering conversion happens separately, preserving exact source facts and enabling targeted labels/suggestions.

### Driver

`CompileError` is the package/orchestration-level envelope for parse, macro, resolve, analysis, module-root, gap/glue, and other compilation-wide failures.

## Analyzer finding flow

One temporary `Analyzer` accumulates its own errors/warnings. `Analyzer::finish` returns them to the driver, which buckets findings by module.

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

Macro-authored tokens carry expansion-origin information used during semantic resolution. Diagnostics continue to use concrete expanded/source spans, while definition-site provenance influences lookup/visibility decisions.

If future diagnostics need explicit expansion traces, that should be layered on the existing origin metadata rather than teaching every semantic error about macro stacks independently.

## Rendering

`omega_diagnostics::Renderer` accepts a `Diagnostic` plus optional `SourceFile` and handles:

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
- Every label in one rendered diagnostic indexes the same `SourceFile`. Cross-file relationships must therefore be represented as separate findings or higher-level context rather than mixing unrelated byte-offset spaces in one label list.
- `SourceFile` precomputes line-start byte offsets. Public positions are 1-based. Display columns count Unicode scalar values with tabs expanded for terminal alignment; they are not grapheme-cluster coordinates. Offsets at or just past EOF clamp rather than panic so end-of-input diagnostics remain renderable.
- `Span` is only a byte range and intentionally carries no file identity. File/module ownership remains with the driver.
- Syntax highlighting is injected through the language-agnostic highlighter interface. Highlight spans must be sorted and non-overlapping, and highlighting must tolerate lexically broken source; an unclassifiable region is simply rendered without a syntax class.
- Multi-line labels reserve a fixed continuation-bar area so source columns do not shift between ordinary and continued lines. Very large labeled ranges elide their middle rather than printing arbitrarily large snippets.
- Color policy belongs at the renderer/CLI boundary. Semantic/compiler crates should not embed terminal escape sequences in diagnostic messages.
