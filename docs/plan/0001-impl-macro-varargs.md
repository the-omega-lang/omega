# Macro system: `$` invocation suffix, no output kind, variadic parameters with repetition

## Task Description

- **What is being asked:** three changes to Omega's macro system, plus the
  supporting work each one turns out to require.
  1. Drop the declared output kind (`=> expr` / `=> items`). A macro
     definition becomes `macro name($p: expr) => { ... }`.
  2. Add variadic macro parameters (`$args: expr...`) and a repetition form
     that expands them, spelled `$...( <separator>? ) { <body> }`.
  3. Change the macro invocation suffix from `!` to `$`: `name$(a, b)`.
- **Purpose:** macros are Omega's only abstraction-over-syntax mechanism and
  the only tool the core library has for generating one `spec ... for T` per
  primitive type (`runtime/core/core/numerics.omg`). Today they can only
  take a fixed arity, which rules out the entire class of "apply this to
  each of N things" macros — the single most common reason a systems
  language reaches for macros at all (logging, assertions, per-type
  registration). This serves *modern syntax with real abstraction power*
  and *abstractions that compile away*: expansion is still a pure
  `SourceModule -> SourceModule` token transform with zero runtime cost, no
  allocation, and nothing surviving into HIR.
- **Reasoning:**
  - *Removing the output kind* is a strict simplification. The declared kind
    is redundant with the invocation's own grammatical position, which
    already determines how an expansion must be parsed. Deleting it removes
    a declaration the author can get wrong, an enum, an error variant, and a
    check — and it fits the already-documented "duck-typed expansion"
    philosophy (`docs/12-macros.md`): a macro body is never validated on its
    own, only as substituted at a concrete call site. After this change the
    *call site* is the single source of truth for how an expansion is
    parsed.
  - *`$` as the invocation suffix* unifies the sigil: `$name`, `$...( ){ }`,
    and `name$( )` are all visibly macro syntax. It also frees `!`
    completely — after this change `!` appears in the grammar only as part
    of `!=`, which makes `!` available for the `bool` logical-not that
    `docs/17-design-review.md:191` flags as a real gap. That gap is *not*
    filled here, but this change stops blocking it.
  - *Repetition spelled `$...(sep){ body }`* rather than `$(sep){ body }`:
    the latter collides with the invocation form, because inside a macro
    body `foo$(a, b)` (a nested call, passed through untouched) and
    `$(,){ ... }` (a repetition) both contain the token pair `$ (`. Telling
    them apart would need a rule about what token precedes the `$`. The
    `$...` spelling is collision-free by construction — a macro argument
    list can never begin with `...` — and it visually ties the repetition to
    the `expr...` declaration that makes it legal.
  - *One variadic parameter, required last.* With that restriction a
    repetition unambiguously iterates *the* variadic and needs no name,
    keeping the syntax light. Naming it later (`$args...(sep){ }`) is a
    purely additive extension if multiple variadics are ever wanted, so this
    restriction closes no doors.
- **Resolved concerns:**
  - **Statement position was missing, and the feature is unusable without
    it.** A repetition like `$...(){ $name($args); }` produces a *statement
    sequence*, which is neither of the two positions macros support today
    (item and expression). Decision: add statement position as a third,
    symmetric expansion mode — an invocation that forms a whole statement
    splices its expansion's statements into the enclosing block, exactly as
    item position splices into the item list. This also means an expanded
    macro can introduce bindings visible to the surrounding code, which the
    alternative (requiring authors to wrap bodies in `{ }`) could not.
  - **Repetition/invocation ambiguity.** Resolved by the `$...` spelling
    above; there is no lookahead heuristic anywhere in this design.
  - **`defer` takes exactly one statement, which a splice cannot fill.**
    `defer name$(...);` is rejected at parse time with a diagnostic telling
    the author to write `defer { name$(...); }`. Uniform rejection, rather
    than "legal only if the expansion happens to be one statement."
  - **Diagnostic regression from dropping the output kind.** The
    `WrongOutputKindForPosition` error disappears. It is replaced by a
    position-aware expansion error ("macro 'x' does not expand to a valid
    expression here: ..."), which names the position the author used and is
    strictly more informative than the old wording.

## Technical Details

### What changes

| Area | File | Change |
| --- | --- | --- |
| Lexing | `compiler/omega-parser/src/lexer.rs` | add `TokenKind::Dollar`; delete `TokenKind::Bang`; `$` not followed by an ident start now lexes as `Dollar` instead of erroring |
| Diagnostics | `compiler/omega-parser/src/diagnostics.rs` | delete `ParseErrorKind::InvalidMetavariable` (now unreachable); add the new macro-syntax error kinds |
| Highlighting | `compiler/omega-parser/src/highlight.rs` | classify `Dollar` as `TokenClass::Keyword`, alongside `Metavar` |
| Macro AST | `compiler/omega-parser/src/ast/statement/macro_definition.rs` | delete `MacroOutputKind`; add `FragmentKind::Ident`; add `MacroSignature`, `MacroBodyPiece`, `MacroRepetition`; `MacroDefinitionStmt.body` becomes a body tree |
| Statement AST | `compiler/omega-parser/src/ast/statement/mod.rs` | add `Statement::MacroInvocation`; update `Item::MacroInvocation`'s doc comment |
| Macro parsing | `compiler/omega-parser/src/parser/macro_syntax.rs` | `$` suffix; signature with variadics; recursive body-tree parsing incl. `$...(sep){ }` |
| Positions | `parser/item.rs`, `parser/expression.rs`, `parser/statement.rs` | dispatch on `Dollar` instead of `Bang`; new statement-position arm; `defer` rejection |
| Block grammar | `compiler/omega-parser/src/parser/expression.rs` | extract `parse_block_contents` out of `parse_codeblock` so statement-list expansion reuses the real block grammar |
| Expansion | `compiler/omega-parser/src/macros.rs` | bindings/rendering rework; three position entry points; statement splicing; new validation |
| HIR | `compiler/omega-hir/src/lower.rs` | `unreachable!()` arm for `Statement::MacroInvocation` |
| Sources | `runtime/core/core/numerics.omg`, `examples/dev/main.omg` | migrate to the new syntax; add a variadic demo to the dev example |
| Docs | `docs/12-macros.md`, `docs/03-control-flow.md`, `docs/17-design-review.md` | rewrite the macro chapter; correct the two `!`-only-appears-in claims |
| Tests | `compiler/omega-parser/tests/macros.rs` (new) | first real test coverage for expansion |

### What must not change

- **The expansion model.** Macros stay a pure `SourceModule -> SourceModule`
  transform in `omega_parser::macros`, run before HIR lowering, with no
  macro node surviving downstream. No hygiene, no gensym, no macro-specific
  type checking — `docs/12-macros.md`'s "Duck-typed expansion" and "Why no
  gensym/hygiene machinery exists" sections stay true verbatim.
- **Spans.** Every token keeps its real originating span; no
  render-to-text-and-relex round trip is introduced. Tokens emitted by a
  repetition are clones carrying their original definition-site or
  call-site spans, exactly like today's substitution.
- **`MacroInvocationExpr`.** Still one shared type across all invocation
  positions, still `args: Vec<Vec<Token>>` (raw token runs). Repetition is a
  property of the *definition body*, not of the argument list.
- **Multiple variadics, named repetitions, nested repetitions, `stmt`/
  `block` fragment kinds, macro-defining macros.** All out of scope.
- **Spans on `MacroError`.** `MacroError` carries names, not spans, and the
  driver reports it without a source snippet
  (`omega_driver::error::DriverError::MacroExpansion`). That is a real
  pre-existing weakness, but fixing it means threading spans through
  `MacroError` and the driver's diagnostic rendering — a separate change.
  Do not start it here; new error variants follow the existing convention.
- **`docs/17-design-review.md`'s actual findings.** Only the factual clause
  about where `!` appears gets corrected; the review's conclusions stay.

### Chosen approach

**1. `$` binds one way in each grammatical role, with no lookahead rules.**

```
$name                 metavariable        (Metavar token, unchanged)
$...( sep? ) { ... }  repetition          (Dollar DotDotDot LParen ... RParen LBrace ... RBrace)
name$( args )         invocation          (Ident Dollar LParen ... RParen)
```

The lexer decides `Metavar` vs `Dollar` on the next character only. The
repetition is recognized by the two-token prefix `$` `...`, which cannot
begin a macro argument, so no context is needed to tell it from an
invocation. Every recognition rule in this design is a fixed-length
lookahead on tokens.

**2. The macro body becomes a tree, parsed once at definition time.**

Today the body is a flat `Vec<Token>` and substitution is a flat scan.
Repetition is inherently nested, so the body gets a minimal tree:

```rust
pub enum MacroBodyPiece {
    /// Any ordinary token, including a `$name` metavariable.
    Token(Token),
    Repetition(MacroRepetition),
}

pub struct MacroRepetition {
    /// Emitted between consecutive expansions, never before the first or
    /// after the last. `None` for `$...(){ ... }`.
    pub separator: Option<Token>,
    pub body: Vec<MacroBodyPiece>,
    pub span: Span,
}
```

This is the key structural decision: repetition is *parsed*, with real
spans and real parse errors at the definition site, rather than
re-discovered by scanning tokens at every expansion. Expansion then becomes
one recursive `render` function over this tree.

**3. Arity is made unrepresentable-if-wrong.**

```rust
pub struct MacroSignature {
    pub fixed: Vec<MacroParam>,
    /// At most one, always last -- enforced by this shape, not by a check.
    pub variadic: Option<MacroParam>,
}
```

`MacroParam` keeps `{ name: Ident, kind: FragmentKind }`; there is no
`variadic: bool` flag to keep consistent with position.

**4. Binding a call site is where "one" and "many" meet, and it is the only
place they do.**

```rust
enum Binding<'a> {
    One(&'a [Token]),
    Many(&'a [Vec<Token>]),
}
```

`render` handles a metavariable bound to `One` by splicing it, and a
repetition by looking up the (single) `Many` binding and re-rendering its
body once per element with that same name rebound to `One(element)`. A
variadic metavariable inside a repetition is therefore an *ordinary*
binding — the repetition is the only construct that knows about plurality
at all.

**5. Position drives parsing, in three symmetric entry points.**

| Position | Parsed with | Result spliced as |
| --- | --- | --- |
| item | `parser::item::parse_source_module` | `Vec<ItemNode>` into the item list |
| statement | `parser::expression::parse_block_contents` | `Vec<StatementNode>` into the enclosing block |
| expression | `parser::expression::parse_expression` | one `ExpressionNode` |

Statement position reuses the *real* block grammar rather than a bespoke
statement-list loop, so a macro body behaves identically to the same text
written inside `{ }`. A block's contents may end in a tail expression; in
statement position a tail is folded into `Statement::Expression`, which is
what makes an expression-bodied macro (`sum_macro$(3, 4);`) still work as a
statement with no special case.

### Risks and open questions

- **Statement vs expression position needs one bounded backtrack.**
  `parse_statement_content` must decide whether `name$(...)` is a whole
  statement (splice) or the start of a larger expression
  (`x = name$(1) + 2`). Use `Parser::mark`/`reset`: parse the invocation,
  keep it as `Statement::MacroInvocation` only if a `;` immediately follows,
  otherwise reset and fall through to the ordinary expression path. `reset`
  already truncates errors, and `parse_codeblock` already backtracks this
  way — do not invent a scanning heuristic instead.
- **A block *tail* invocation stays expression position.** In
  `{ ...; name$(a) }` the invocation has no `;`, so it is the block's tail
  expression, which is correct and consistent. Flag it if the distinction
  seems to surprise a real test case rather than "fixing" it locally.
- **`examples/dev/main.omg` is the de facto integration test** — it is
  ~1500 lines and compiled by `just build-exe`. If a migration edit there
  causes an unrelated failure, report it rather than working around it.
- **Deleting `TokenKind::Bang`** makes a bare `!` an
  `InvalidCharacter('!')` lex error. If any `.omg` source outside the two
  migrated files uses `!`, stop and report instead of inventing syntax.

## Implementation Plan

Each step leaves the tree building (`cargo build`) and the runtime + example
compiling (`just build-exe`).

### Step 1 — invocation suffix `!` -> `$`

1. `lexer.rs`: add `Dollar` to `TokenKind` (doc comment: `$` as the
   invocation suffix `name$(...)` and as the repetition prefix `$...`;
   `$name` is still lexed atomically as `Metavar`). Add its `describe()`
   arm (`"'$'"`). Delete the `Bang` variant, its `describe()` arm, and the
   `'!' => TokenKind::Bang` arm in `scan_punct`. Update `TokenKind::
   Metavar`'s doc comment, which currently claims `$` has exactly one use.
2. `lexer.rs::scan_token`: change the `'$'` arm to peek at the next
   character — ident start -> `scan_metavar`, otherwise consume the `$` and
   return `Dollar`. `scan_metavar` no longer needs its error path; drop it
   and the now-unreachable `ParseErrorKind::InvalidMetavariable` (its
   variant, its `Display` arm, and its arm in `diagnostics.rs`'s
   `ParseError::to_diagnostic`, around `diagnostics.rs:61`).
3. `parser/macro_syntax.rs::parse_macro_invocation`: expect `Dollar`
   (`"'$'"`) instead of `Bang`.
4. `parser/item.rs:137` and `parser/expression.rs:526`: match
   `TokenKind::Dollar` at `peek_at(1)`.
5. `highlight.rs`: add `TokenKind::Dollar` to the `TokenClass::Keyword`
   group next to `Metavar`.
6. Migrate sources: `runtime/core/core/numerics.omg` (12 invocations,
   `signed_integer!(i8)` -> `signed_integer$(i8)`) and
   `examples/dev/main.omg` (`sum_macro!`, `make_point_type!` and their
   surrounding comments).
7. Verify with `just build-exe`.

### Step 2 — remove the output kind

1. `ast/statement/macro_definition.rs`: delete `MacroOutputKind`; remove
   `MacroDefinitionStmt.output`.
2. `parser/macro_syntax.rs`: delete `parse_macro_output_kind`;
   `parse_macro_definition` goes `)` -> `=>` -> `{` directly.
3. `macros.rs`: delete `MacroError::WrongOutputKindForPosition` and both
   `def.output != ...` checks. Add a `MacroPosition { Item, Statement,
   Expression }` enum (Statement is unused until step 3 — add it there if
   preferred) and give `ExpansionParseError` a `position` field, so its
   `Display` reads e.g. `macro 'sum_macro' does not expand to a valid
   expression here: <errors>`.
4. Migrate `runtime/core/core/numerics.omg` (3 definitions) and
   `examples/dev/main.omg` (2 definitions) to `=> {`.
5. `prelude.rs`: drop the `MacroOutputKind` re-export if present.

### Step 3 — statement position

1. `parser/expression.rs`: extract the body of `parse_codeblock`'s
   `allow_struct_literals` closure into
   `pub fn parse_block_contents(p: &mut Parser) -> CodeblockExpr` (the
   statement/tail loop, stopping at `RBrace` or EOF, *without* consuming
   either brace). `parse_codeblock` becomes
   `allow_struct_literals(|p| { expect('{'); let cb = parse_block_contents(p); expect('}'); Some(cb) })`.
   Behavior must be identical.
2. `ast/statement/mod.rs`: add
   `Statement::MacroInvocation(MacroInvocationExpr)` with a doc comment
   explaining that the expansion pass splices its statements in place, and
   that it never reaches HIR.
3. `parser/statement.rs::parse_statement_content`: add an arm for
   `TokenKind::Ident(_)` with `peek_at(1) == Dollar` — `mark`, parse the
   invocation, and if `p.check(&TokenKind::Semi)` return
   `(Statement::MacroInvocation(inv), false)`; otherwise `reset` and fall
   through to the existing expression path. Place it before the generic
   `Ident` handling.
4. `parser/statement.rs`, `defer` arm: if the parsed inner content is
   `Statement::MacroInvocation`, report a new
   `ParseErrorKind::MacroInvocationNotAllowedAfterDefer` ("a macro
   invocation can expand to more than one statement; write
   `defer { name$(...); }`") and return `None`. Add the variant, its
   `Display`, and its `to_diagnostic` arm with that suggestion as help
   text.
5. `omega-hir/src/lower.rs`: add a `Statement::MacroInvocation(_) =>
   unreachable!(...)` arm in `lower_stmt`, worded like the existing
   `Expression::MacroInvocation` arm at line 747.
6. `macros.rs`: add `expand_statements_invocation` (parse the substituted
   tokens with `p.allow_struct_literals(parse_block_contents)`, require
   `p.is_eof()` and no errors, then convert: each statement, plus the tail
   — if any — as a trailing `Statement::Expression`). Change
   `expand_codeblock` to splice: iterate `cb.statements`, expanding a
   `Statement::MacroInvocation` into many and recursing into the results
   via `expand_stmt_node`. `expand_statement`'s own
   `Statement::MacroInvocation` arm is `unreachable!()` (the splicing
   parent handles it; `defer` was rejected at parse time; `ForStmt::init`
   has its own narrow grammar and can never produce one).

### Step 4 — `ident` fragment kind

1. `ast/statement/macro_definition.rs`: add `FragmentKind::Ident`, and
   update the type's doc comment (it currently cites `ident` as a
   hypothetical).
2. `parser/macro_syntax.rs::parse_fragment_kind`: recognize the contextual
   `ident`; update the expectation string to `"'expr', 'type' or 'ident'"`.
3. `macros.rs::validate_fragment`: `FragmentKind::Ident` validates via
   `p.expect_ident()` plus the existing fully-consumed/no-errors check — a
   single identifier token, deliberately not a path.

### Step 5 — variadic parameters

1. `ast/statement/macro_definition.rs`: add `MacroSignature { fixed,
   variadic }` as described above; `MacroDefinitionStmt.params` becomes
   `signature: MacroSignature`.
2. `parser/macro_syntax.rs`: rename `parse_macro_params` ->
   `parse_macro_signature`. Per parameter: `Metavar`, `:`, fragment kind,
   then an optional `DotDotDot` marking it variadic. If a variadic
   parameter is followed by a `,`, report a new
   `ParseErrorKind::VariadicMacroParamNotLast` ("a variadic macro
   parameter must be the last one, and a macro can have at most one") and
   return `None`. Add its `Display`/`to_diagnostic` arms.
3. `macros.rs`: replace the `args.len() != params.len()` check with one
   driven by the signature. Change `MacroError::ArgCountMismatch`'s
   `expected: usize` to `expected: Arity` where
   `enum Arity { Exact(usize), AtLeast(usize) }`, whose `Display` renders
   "expects 2 argument(s)" / "expects at least 1 argument(s)". Fragment
   validation runs on each variadic element individually against the
   variadic parameter's kind.
4. Until step 7 a variadic parameter has no way to be used in a body; that
   is fine and buildable — declaring one and referencing it is rejected by
   step 7's validation, and until then by the existing metavariable check
   only if misspelled. Do not ship this step alone.

### Step 6 — macro body tree (pure refactor, no new syntax)

1. `ast/statement/macro_definition.rs`: add `MacroBodyPiece` and
   `MacroRepetition` (definitions above); `MacroDefinitionStmt.body`
   becomes `Vec<MacroBodyPiece>`.
2. `parser/macro_syntax.rs`: replace the body's use of `capture_token_run`
   with `parse_macro_body(p, in_repetition: bool) -> Vec<MacroBodyPiece>`,
   which keeps the existing bracket-depth logic for ordinary tokens and
   stops at the depth-0 `}` that closes it. In this step it only ever
   produces `Token` pieces. `capture_token_run` stays, now used solely by
   `parse_macro_args`; narrow its doc comment accordingly.
3. `macros.rs`: `substitute_tokens` becomes
   `render(body: &[MacroBodyPiece], bindings: &Bindings, out: &mut Vec<Token>)`,
   and `validate_body_metavars` walks pieces instead of tokens. Behavior is
   unchanged in this step.

### Step 7 — repetition

1. `parser/macro_syntax.rs`: in `parse_macro_body`, on `Dollar` followed by
   `DotDotDot`, call `parse_repetition`:
   - consume `$`, `...`, `(`;
   - separator: nothing, or exactly one token that is not a delimiter
     (`( ) [ ] { }`). Anything else reports a new
     `ParseErrorKind::InvalidMacroSeparator` ("a macro repetition separator
     must be a single non-bracket token, e.g. `$...(,){ ... }`");
   - consume `)`, `{`, recurse `parse_macro_body(p, true)`, consume `}`;
   - record the whole construct's span.
   - If `in_repetition` is already true, report a new
     `ParseErrorKind::NestedMacroRepetition` ("macro repetitions can't
     nest; a macro has at most one variadic parameter") and return `None`.
2. `macros.rs`: add the `Binding` enum and a small `Bindings` type wrapping
   `HashMap<Ident, Binding<'a>>` with `lookup` and
   `with_element(name, tokens) -> Bindings` (clone + override) helpers.
   `bind_arguments(def, args)` builds it: fixed parameters -> `One`,
   variadic -> `Many` over the remaining args.
3. `macros.rs::render`: `Repetition` looks up the definition's variadic
   parameter, expects `Many`, and for each element renders the repetition
   body with `bindings.with_element(variadic_name, element)`, pushing a
   clone of the separator between (never before the first, never after the
   last). Zero elements render to nothing.
4. `macros.rs`: extend definition-time validation (the pass currently named
   `validate_body_metavars`, now `validate_definition`) with three checks,
   all reported as new `MacroError` variants:
   - `VariadicOutsideRepetition { macro_name, metavar }` — the variadic
     parameter is referenced outside any repetition.
   - `RepetitionWithoutVariadic { macro_name }` — a repetition in a macro
     that declares no variadic parameter.
   - `RepetitionMissingVariadic { macro_name }` — a repetition whose body
     never references the variadic (it would emit N identical copies, which
     is always a bug).
   Keep the existing `UnknownMetavariable` check.

### Step 8 — sources, docs, example

1. `examples/dev/main.omg`: add a variadic demo exercising both shapes —
   a statement-splicing macro (`$...(){ $f($args); }`) and an
   argument-list macro (`$f($...(,){ $args })`) — called from `main` with
   its output visible in the program's stdout, alongside the existing
   `sum_macro`/`make_point_type` demos.
2. `docs/12-macros.md`: rewrite. Cover the new definition form, the three
   invocation positions and what each splices, fragment kinds
   (`expr`/`type`/`ident`), the one-variadic-last rule, repetition syntax
   and separator semantics, and the empty-variadic case. Keep the existing
   "Mechanism" / "Duck-typed expansion" / "Why no gensym" / "Where it's
   actually used" sections, updated where they mention output kinds.
3. `docs/03-control-flow.md:110` and `docs/17-design-review.md:191`: both
   claim `!` appears only in `!=` or `name!(...)`. Correct them to `!=`
   only. In the design-review doc, correct the factual clause only — leave
   the finding itself intact, and note that `!` is now unallocated.
4. `grep -rn '!(' docs/ README.md` for any remaining old-syntax examples
   (ignore Rust `todo!()`/`unreachable!()` mentions).

## Testing

### New cases — `compiler/omega-parser/tests/macros.rs` (new file)

`omega_parser::SourceModule::parse` and `omega_parser::macros::expand` are
both public, so each case is "parse this source, expand it, assert on the
result or the error." This is the first test coverage `macros.rs` has had;
it is part of the deliverable, not optional.

- **Positions:** an item-position macro expanding to two items; a
  statement-position macro expanding to two statements (assert both land in
  the enclosing block, in order); an expression-position macro; an
  expression-bodied macro used as a statement (tail folded into
  `Statement::Expression`); a statement-position invocation nested in a
  larger expression (`x = m$(1) + 2`) still parsing as an expression.
- **Variadics:** zero, one, and several variadic arguments; separator
  emitted only *between* elements; no separator (`$...(){ }`); a repetition
  with fixed parameters referenced inside it alongside the variadic; a
  macro with only a variadic parameter.
- **Fragments:** `ident` accepting a bare identifier; `type` still
  accepting `*mut u8`; `expr` unchanged.
- **Nesting:** a macro invoked inside another macro's expansion still
  expands (existing behavior, now across all three positions).
- **Lexing:** `name$(` , `$...(` and `$name` all lex as expected; a bare
  `$` lexes as `Dollar` rather than erroring.

### Negative cases — each must fail, with the stated diagnostic

- `macro m($a: expr..., $b: expr) => { }` -> *a variadic macro parameter
  must be the last one, and a macro can have at most one*.
- `$...(){ $...(){ $x } }` -> *macro repetitions can't nest*.
- `$...( ( ){ $x }` / two separator tokens -> *a macro repetition separator
  must be a single non-bracket token*.
- A variadic metavariable used outside a repetition -> *`VariadicOutsideRepetition`*.
- A repetition in a macro with no variadic parameter ->
  *`RepetitionWithoutVariadic`*.
- A repetition whose body never mentions the variadic ->
  *`RepetitionMissingVariadic`*.
- Too few arguments for a variadic macro -> *expects at least N argument(s)*.
- An argument that doesn't match its fragment kind (e.g. `3 + 4` for an
  `ident` parameter) -> existing `FragmentMismatch`.
- A macro whose body is a statement sequence used in expression position ->
  *macro 'm' does not expand to a valid expression here: ...* (this is the
  path that replaces `WrongOutputKindForPosition`; confirm the message
  names the position).
- `defer m$(x);` -> *a macro invocation can expand to more than one
  statement; write `defer { m$(x); }`*.
- Old syntax must now fail cleanly: `m!(1)` and `macro m() => expr { }`.

### Regression risk

- `runtime/core/core/numerics.omg` is the highest-value regression check —
  12 invocations across 3 macros generating every numeric type's spec
  impls. `just build-core` failing means expansion regressed.
- `examples/dev/main.omg` exercises both existing macros plus everything
  else in the language; `just build-exe` then `just run-exec` must produce
  the same output as before, plus the new variadic demo lines.
- `compiler/omega-mangle/tests/roundtrip.rs` is unrelated and must keep
  passing (`cargo test`).
- Highest-risk edits: the `parse_codeblock` extraction in step 3.1 (a
  behavior change there breaks every block in the language, not just
  macros) and the `mark`/`reset` statement arm in step 3.3 (a missed
  `reset` would swallow tokens or leak stale errors).

### Target coverage

None specific — this is a front-end-only change with no runtime, codegen,
or ABI surface. Expansion still finishes before HIR lowering, so freestanding
and no-allocator targets are unaffected by construction.
