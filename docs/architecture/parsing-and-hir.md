# Parsing, macro expansion, and HIR

This document describes the current frontend boundary implemented by `omega-parser` and `omega-hir`.

```text
source text
   -> lexer
   -> tokens
   -> recursive-descent parser
   -> AST
   -> macro expansion (token substitution + position-aware reparse)
   -> expanded AST
   -> HIR lowering
   -> HIR with stable identities
```

The normative lexical and grammatical rules live in [`../language/lexical-structure.md`](../language/lexical-structure.md) and [`../language/grammar.md`](../language/grammar.md). This file explains how the compiler realizes them.

## `omega-parser` ownership

`omega-parser` owns:

- lexical tokenization (`lexer.rs`);
- parser AST shapes (`ast/`);
- recursive-descent grammar (`parser/`);
- parser recovery and parse diagnostics;
- source macro definitions/invocations and expansion (`macros.rs`);
- Omega syntax highlighting (`highlight.rs`);
- the public parser-facing re-export surface (`prelude.rs`).

It does **not** own:

- durable compiler node IDs;
- name/type resolution;
- semantic validity requiring declarations or types;
- aggregate layout or backend representation.

## Lexing

The lexer produces tokens with source spans and, after macro expansion begins, token-origin information. Token spelling belongs to `TokenKind`; parser code should not maintain parallel spelling tables.

Contextual words are intentionally not all hard lexer keywords. The parser's `parser::contextual` registry owns words whose keyword meaning depends on grammar position.

### Contextual-keyword commit rule

A contextual word must be treated as syntax only after the surrounding shape proves that interpretation. A bare identifier spelling such as `mut`, `comp`, or `root` remains usable as a name where the contextual production does not match.

This rule matters because committing on the word alone silently shrinks the identifier namespace instead of merely producing a local parse error.

## Parser organization

The recursive-descent parser is split by syntactic concern:

- `parser/expression.rs` — expressions and precedence;
- `parser/item.rs` — top-level/member declarations;
- `parser/statement.rs` — statement grammar;
- `parser/type.rs` — type grammar;
- `parser/macro_syntax.rs` — macro definition/invocation syntax;
- `parser/recovery.rs` — synchronization after parse errors;
- `parser/contextual.rs` — contextual keyword facts.

The AST under `ast/` is source-oriented. It intentionally represents syntax before semantic interpretation. For example, nested field/index/deref forms are still syntactic expression shapes rather than a semantic “place”.

## AST and spans

`SourceModule::parse` is the ordinary source -> AST entry point.

A construct that can be diagnosed carries a span appropriate to that construct, including more specific spans such as names/signatures/return types where later diagnostics need them. Do not replace specific child spans with an enclosing item's span merely because the parent already covers the same bytes.

Spans are byte offsets into the retained source text; rendering is handled by `omega-diagnostics` above the parser.

## Macro expansion boundary

Macro expansion happens **after an initial parse** and **before HIR lowering**.

This is structural, not incidental. A macro body is captured as token-oriented syntax, substituted, then reparsed through the ordinary parser at the invocation's syntactic position:

- item position;
- statement position;
- expression position.

The parser is therefore the authority on whether an expansion forms valid syntax in its position.

### Expansion environment

The driver constructs the module-visible macro environment. `omega_parser::macros` then merges local definitions and expands invocations.

`ExpansionState` records definition-site provenance for macro-authored tokens:

- defining module;
- macro visibility;
- per-module macro environments used for nested expansion.

This provenance later lets semantic path resolution honor definition-site lookup and ensure a macro does not expose a dependency narrower than the macro itself.

Tokens substituted from macro arguments retain caller-side origin; tokens emitted by the macro body receive the expansion's definition-site origin. That distinction is what prevents the whole expanded subtree from being incorrectly treated as either caller-authored or definition-authored.

### Expansion limits

Expansion is budgeted so runaway recursive expansion becomes a structured macro error rather than unbounded recursion. The exact budget is an implementation safety limit, not language semantics.

## Why HIR exists

Macro expansion can splice and reparse arbitrary syntax. Therefore an identity assigned to a pre-expansion AST node is not stable: the node may disappear, move, or be replaced.

`omega-hir` begins only after expansion and provides the earliest durable node identity.

```rust
ModuleId(u32)
HirId { module: ModuleId, local: u32 }
```

Real-source `HirId`s are minted by a per-module `HirIdGen` during lowering. There is no global counter in `omega-hir`.

`SYNTHETIC_MODULE` is reserved for IDs minted later by the driver for semantic artifacts that have no direct source HIR node, such as concrete generic instantiations/spec-default method materializations.

## HIR lowering contract

`omega_hir::lower_module(ModuleId, &SourceModule) -> HirModule` is **infallible**.

HIR lowering may perform structural transforms that require no semantic facts, but it may not reject a program for a name/type rule. Rejectable questions belong to semantic analysis.

### Current HIR desugarings

HIR lowering owns four important source-shape normalizations:

1. **Synthetic `self` insertion.** Member functions receive the parameter shape implied by `SelfMode`.
2. **By-value `mut self` shadowing.** It becomes an implicit mutable local shadow at the beginning of the body, avoiding a separate downstream “mutable parameter” concept.
3. **`spec T` parameter lowering.** Static-spec parameter sugar becomes an ordinary fresh bound generic parameter, so later phases reason through one generic mechanism.
4. **Place-chain flattening.** Nested field/index/deref AST expressions that form an assignable/addressable place become `HirPlace { root, projections }`.

These transforms are deliberately centralized so the analyzer, MIR and backends do not each recognize the same sugar independently.

## HIR shape

HIR remains close to syntax. It still carries unresolved:

- `Type` syntax;
- source paths;
- annotations;
- generics/bounds;
- declaration and expression structure.

This is intentional. HIR is an **identity + structural normalization boundary**, not a typed IR.

## Places

The parser does not have a semantic place abstraction. HIR lowering recognizes place-shaped syntax and flattens it into:

```text
root
  + projection 0
  + projection 1
  + ...
```

Later semantic analysis resolves each projection's type, mutability, field identity/index, alignment, and addressability. MIR then maps the already-resolved place to local/global/expression storage.

## Frontend error ownership

- Invalid token/lexical form -> parser diagnostics.
- Invalid grammar -> parser diagnostics.
- Invalid macro definition/invocation/expanded syntax -> macro error.
- Invalid name/type/visibility/conformance/etc. -> analyzer/driver.

A new check should live at the earliest phase that has **all facts needed to decide it**, but not earlier.

## Change routing

### Grammar-only change

Usually inspect:

- relevant `parser/*.rs`;
- AST shape;
- parser tests;
- language grammar docs.

Only touch HIR if the source shape stored downstream changes.

### New syntax with existing semantics

Prefer desugaring at HIR when the transformation is syntax-only and lets later phases reuse an existing semantic mechanism.

### Macro behavior

Start in `macros.rs` plus the parser entry point for the expansion position. Cross into the driver only if macro visibility/environment construction changes.

### New identity-bearing construct

Ensure the construct receives a HIR ID and useful specific spans during lowering. Do not mint ordinary source-node `HirId`s downstream.

## Frontend implementation invariants

The source previously carried these facts as scattered Rust doc comments. They are consolidated here because they are durable frontend architecture rather than local API documentation.

### Parser state and backtracking

- The token stream always ends in an `Eof` sentinel. Parser lookahead clamps to that sentinel and `advance` does not consume it, so recovery and speculative parsing can safely observe EOF repeatedly.
- `Parser::mark` / `Parser::reset` is the limited backtracking mechanism. Resetting also discards diagnostics emitted after the mark, so an abandoned speculative parse cannot leak errors. The main use is code-block tail-expression versus statement disambiguation; ordinary grammar choices should prefer bounded lookahead.
- Nested generic closers reuse the lexer's maximal-munch `>>` token. The parser may split it into two synthetic `>` observations via `pending_gt`; `last_span` therefore tracks the last consumed token explicitly rather than deriving it from the immutable token slice.
- Recursive expression/type descent shares `MAX_NESTING_DEPTH`. The limit protects the native stack and indirectly bounds later AST/HIR traversal depth. It is an implementation safety limit, not a language-semantic maximum.

### Ambiguous braces and contextual syntax

In `if`/`while`/`for` condition positions, an immediately following `{` must be available to start the body, so bare struct-literal syntax is temporarily restricted. Bracketed subexpressions restore normal struct-literal parsing because the body brace can no longer be confused with a literal brace. Keep this state restoration scoped: parser functions have many early-return paths.

Contextual words must likewise be committed only after the surrounding token shape proves their grammatical role. Adding a contextual construct must not accidentally reserve that spelling in unrelated identifier positions.

### AST representation

The AST is deliberately syntax-shaped. Semantic facts such as addressability, resolved declarations, types, and conformance do not belong in parser nodes. Paths additionally carry macro-resolution provenance, but structural path/type comparisons remain based on source structure rather than provenance; provenance affects later lookup, not syntactic identity.

Specific child spans are intentional. Fields, parameters, names, signatures, and return types retain their own spans so later diagnostics can underline the smallest honest region instead of an enclosing declaration.

### Macro spans and provenance

`Span` itself has no source-file identity. A macro definition may come from another module, so definition-module byte offsets cannot safely survive as ordinary spans inside the caller's expanded AST: rendering them against the caller's source would point at unrelated text. Macro-authored generated tokens therefore use invocation/call-site spans for diagnostics while separate origin metadata preserves definition-site module/visibility provenance for resolution. Substituted argument tokens retain caller provenance.

The macro body is not independently type-checked or semantically validated. Expansion substitutes tokens and reparses them at the invocation's syntactic position; normal downstream analysis validates the resulting program.

### HIR identity and synthetic nodes

Real-source `HirId`s are minted only during post-expansion HIR lowering from a per-module counter. Downstream phases must not invent ordinary source IDs. Semantic artifacts without source HIR nodes use the driver's reserved synthetic module identity, keeping synthetic IDs disjoint from real module IDs.

Synthetic nodes may have no token of their own. For example, the implicit `self` parameter uses the enclosing function's span because there is no source token to point at. This is preferable to manufacturing a meaningless byte offset.

### HIR structural lowering

HIR lowering may normalize source structure only when no semantic knowledge is required. Current examples include synthetic `self`, by-value `mut self` shadowing, static-spec parameter desugaring to fresh bound generics, and flattening place-shaped field/index/deref chains. If a chain is rooted in a non-place expression (for example `make().field`), that expression remains the place root and projections are appended in source order.
