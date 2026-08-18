# Parsing, macro expansion, and the HIR

The front half of the pipeline, documented in the same style as
[mir-and-codegen.md](mir-and-codegen.md) does the back half. Three
crates: `omega-diagnostics` (the root of the dependency graph — spans,
findings, the renderer), `omega-parser` (lexer, recursive-descent parser,
macro expansion), and `omega-hir` (identity assignment and four
desugarings).

## Why this exists

Source text reaches the analyzer through four passes, each with a single
job:

```
text ──lex──▶ tokens ──parse──▶ AST ──expand──▶ AST ──lower──▶ HIR
```

The two non-obvious boundaries:

- **Macro expansion sits between two ASTs, not before parsing.** A macro
  body is captured as a token tree, substituted at the invocation, and fed
  back into the ordinary parser at whichever entry point the invocation's
  *position* calls for (item, statement, or expression). Token-level
  expansion before parsing cannot work, because the choice of entry point
  is a syntactic fact only the parser knows.
- **HIR exists because expansion invalidates identity.** Expansion splices
  and re-parses, so any id assigned before it would be meaningless
  afterward. HIR is the earliest point where a node can get an identity
  that survives to codegen — which is what `HirId` is, and what the
  analyzer, MIR, codegen and the driver's monomorphization cache all key
  on.

## One fact, one home

The recurring failure mode in this part of the compiler is not bad logic —
it is a fact written down in more than one place, where the copies drift.
Each row below is now a single table or helper; the "was" column is what it
looked like before.

| Fact | Was | Is |
|---|---|---|
| A token's source spelling | `KEYWORDS`, `MULTI_CHAR_PUNCT`, `scan_punct`'s match, `describe()` | `TokenKind::spelling()` |
| Which words are contextually reserved | 33 scattered `name == "…"` literals | `parser::contextual` |
| Operator precedence | 6 near-identical tier functions + a prose comment | one tier table |
| An aggregate's body grammar | 4 field loops + 5 function loops | `parse_aggregate_fields` / `parse_member_functions` |
| Where a construct is in the source | its *parent's* wrapper node | the construct itself |

## Contextual keywords

Omega reserves as few words as it can. Eighteen words are keywords only at
one grammar position and ordinary identifiers everywhere else; the full set
lives in `omega_parser::parser::contextual`, one `const` each with a note on
where it is reserved:

`mut`, `comp`, `self`, `reveal`, `sizeof`, `in`, `exposed`, `internal`,
`marker`, `gap`, `glue`, `conform`, `to`, `primitive`, `root`, and the macro
fragment kinds `expr`, `type`, `ident`.

**The commit rule.** A contextual keyword is only committed to once the
*whole shape* around it is confirmed — never on the bare word. `mut` leads a
binding only when `[mut] [comp] ident (':='|':')` matches in full; `marker`,
`gap`, `glue` only when followed by another identifier; `root` only when
followed by `::`. Breaking this rule does not produce a parse error at the
keyword — it silently narrows the language, because the word stops being
usable as a name.

Three words were violating it and have been fixed:

- `reveal` and `comp` are prefix operators spelled as identifiers, so
  `return comp;` parsed `comp` as the operator and then failed looking for
  its operand. A variable named `comp` could be declared but never read.
  Both now yield to the identifier reading when no expression follows.
- `exposed` / `internal` were consumed as visibility modifiers before the
  following shape was checked, so a field or binding named `exposed`
  (`exposed: i32;`) did not parse at all.

`tests/contextual_keywords.rs` is driven by the registry itself, so a newly
added contextual keyword is covered the moment it is registered.

## Span ownership

**A construct that can be the subject of a diagnostic owns its own span.**

This was not previously true. Spans lived only on the parser's wrapper nodes
(`ItemNode`, `StatementNode`, `ExpressionNode`), and anything never wrapped
in one — every method, field, parameter and spec function — inherited its
parent's. Two user-visible consequences, both fixed:

```
error: 'x' is declared multiple times in this scope    ← before
 1 |   struct Point {
   |  _^
 2 | |     x: i32;
...  |
13 | | }
   | |_^ `x` declared again here
```

Both labels, primary *and* secondary, covered the whole struct. And:

```
error: mismatched types: expected return type 'i32', found 'void'    ← before
1 |   sum(a: i32, b: i32) => i32 {
  |  _^
... |
4 | | }
  | |_^ expected `i32` because of the declared return type, found `void`
```

The return-type mismatch underlined the entire body rather than the `i32`
that was actually written. Both now point at exactly the offending token
run.

The spans a construct carries are named for what a diagnostic wants:
`name_span` (identity problems — declared twice, collides with a variant),
`signature_span` (name through return type, excluding the body), and
`return_type_span`. They are threaded parser → HIR → analyzer, including
through `RawSpecFunctionSig` so a spec default method reconstructed for body
checking keeps the spec function's real spans.

## What lowering owns

Exactly four desugarings, all of which need no type information — which is
precisely why they belong here rather than being done ad hoc by whichever
analyzer pass needed one first:

1. **`self` insertion** — the synthetic `self: *Self` parameter, shaped by
   `SelfMode`. Always `Self`, never the owner's own name, so a generic
   owner needs no type arguments supplied to resolve it.
2. **`mut self` shadowing** — by-value `mut self` becomes an implicit
   `mut self := self;` as the body's first statement, so nothing downstream
   needs a notion of a mutable parameter.
3. **`spec T` parameters** — `f(x: spec Foo)` becomes `f<$Param0: Foo>(x:
   $Param0)`. Every occurrence gets its own fresh generic, matching Rust's
   `impl Trait`. Nothing after lowering sees `Type::SpecStatic` in
   parameter position.
4. **Place-chain flattening** — the parser's nested
   `FieldAccess`/`Index`/`Deref` become one `HirPlace`: a root plus a flat
   projection list in source order. The parser has no notion of an
   addressable location at all.

Lowering is **infallible**. Every rejectable question is the analyzer's,
which keeps "can this program be rejected here?" answerable per pass rather
than per call site.

## Illegal states stay illegal

`RangeEnd` is an enum (`Inclusive` / `Exclusive` / `Open`) rather than
`end: Option<Expr>` plus `inclusive: bool`, so "an inclusive range with no
end" is unrepresentable rather than merely rejected at runtime. Lowering
used to flatten it straight back into the rejected shape, so HIR could hold
a state the grammar cannot produce. `HirRangeEnd` now mirrors it, and the
guarantee holds the whole way down.
