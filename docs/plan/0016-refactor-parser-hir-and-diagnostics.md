# Refactor: `omega-parser`, `omega-diagnostics`, `omega-hir`

## Task Description

- **What is being asked:** A structural clean-up of the front three crates —
  `omega-diagnostics`, `omega-parser`, `omega-hir` — against twelve stated
  goals (lower cognitive load, more declarative code, less technical debt,
  fewer hacks, fewer bugs, simpler logic, better abstractions/modularity/
  architecture, better future-proofing), plus a written record in `docs/` of
  the language-level questions this pass uncovered but must not decide
  unilaterally.

- **Purpose:** These three crates are the compiler's foundation: everything
  downstream reads their types. Debt here is the most expensive kind, because
  every later pass inherits both the data shapes and the diagnostic quality.
  Two of the findings below are not stylistic at all — they are confirmed,
  reproducible defects in user-facing output.

- **Reasoning:** The overwhelming majority of what is wrong in these crates
  is *not* bad logic. The logic is good and unusually well documented. What
  is wrong is that a handful of facts are **written down in more than one
  place**, and a handful more are **not written down anywhere**:

  | Fact | Written down in | Should be |
  |---|---|---|
  | A token's source spelling | `KEYWORDS`, `MULTI_CHAR_PUNCT`, `scan_punct`'s match, `describe()` | one table |
  | Which words are contextually reserved | 33 scattered `name == "…"` literals | one table |
  | Operator precedence | 6 near-identical tier functions + a prose comment | one table |
  | What one parse error *is* | the enum variant, `Display`, `to_diagnostic` | one place |
  | An aggregate's body grammar | 4 field loops + 5 function loops | one helper each |
  | Where a construct is in the source | its *parent's* wrapper node | the construct |

  Every refactor below is an instance of that one principle. Nothing here
  changes which programs compile.

- **Resolved concerns:**

  1. **"Mirror, don't unify" (docs/README.md).** The project explicitly
     treats `struct`/`enum`/`union` as three separate hand-mirrored item
     pipelines, and calls that a style choice, not debt. This plan does
     **not** touch that. Sharing `parse_aggregate_fields` between them shares
     a *grammar production* — a purely syntactic fact that is genuinely
     identical in all three — while `StructStmt`/`UnionStmt`/`EnumStmt`,
     `HirStructDef`/`HirUnionDef`/`HirEnumDef` and every analyzer/codegen
     pipeline stay exactly as separate as they are today. If an executing
     agent finds itself merging two *item types*, it has overshot; stop and
     ask.

  2. **Does `omega-hir` earn its existence?** It is ~95% a mechanical clone
     of the AST, which invites deleting it and having the analyzer read the
     AST directly. It should stay, for a reason that is currently written
     down nowhere: **HIR is the first tree that exists after macro
     expansion.** `macros::expand` splices tokens and re-parses, so any id
     minted before expansion would be invalidated by it. HIR is where stable
     `HirId`s can first be assigned, and it owns four real desugarings
     (`self` insertion, `mut self` shadowing, `spec T` → generic param,
     place-chain flattening). Step I.1 writes that justification down so the
     question stops recurring.

  3. **Is span work a refactor or a feature?** It is a bug fix. Reproduced
     below. The cause is architectural (span lives on the wrapper node, so
     any construct that isn't wrapped inherits its parent's), which is why it
     belongs in this pass rather than a diagnostics-polish pass.

## Technical Details

### Confirmed defects (reproduced against `HEAD`)

**D1 — every diagnostic anchored on a method or field underlines the whole
type declaration.** `DeclarationStmt` and `FunctionDefinitionStmt` carry no
span of their own; only the wrapping `ItemNode`/`StatementNode` does, and
struct members are never wrapped in one. `omega_hir::lower` therefore passes
the enclosing struct's span down (see `lower.rs`'s own note on
`lower_function_def`, "an approximation but strictly better than nothing").

```
struct Point { x: i32; y: i32; x: i32; ... }
```
```
error: 'x' is declared multiple times in this scope
 1 |   struct Point {
   |  _^
 2 | |     x: i32;
...  |
13 | | }
   | |_^ `x` declared again here
 1 |   struct Point {
   |  _-
...
   | |_- `x` first declared here
```

Both labels — primary *and* secondary — cover the same 13 lines. rustc would
point at two columns.

**D2 — a return-type mismatch underlines the entire function body** instead
of the declared return type, at top level as well as in methods:

```
sum(a: i32, b: i32) => i32 { a; b; }
```
```
error: mismatched types: expected return type 'i32', found 'void'
1 |   sum(a: i32, b: i32) => i32 {
  |  _^
... |
4 | | }
  | |_^ expected `i32` because of the declared return type, found `void`
```

`Analyzer::check_return_type` (`omega-analyzer/src/analysis/items.rs:530`)
is handed `HirFunctionDef::span`, which is all there is.

**D3 — `HirRange` re-introduces an illegal state the AST deliberately
eliminated.** `ast::range::RangeEnd`'s own doc comment says making it an enum
"instead of `end: Option<Expr> + inclusive: bool` (the old shape) means an
inclusive/exclusive range with no end … are no longer representable at all
rather than merely rejected by a runtime check." `HirRange` (`hir.rs:668`)
then flattens it straight back to `end: Option<Box<…>>, inclusive: bool`.

**D4 — non-associative comparison has no diagnostic of its own.** The
grammar enforces it structurally (`parse_comparison` matches at most one
operator), but the resulting message is unrelated to the rule:

```
a := b < c < d;   →   error: expected ';', found '<'
```

**D5 — inconsistent error recovery between item bodies.** `struct`, `union`
and `enum` recover from a malformed member via
`recovery::synchronize_to_statement_boundary` and keep parsing the rest of
the body. `conform`, `primitive`, `gap` and `glue` use `?` and abandon the
whole item on the first bad member. Same construct, two behaviours, no
stated reason.

**D6 — `ParseErrorKind::Expected` used as a general-purpose string error.**
`parse_glue_def` reports `Expected { expected: "a non-generic, static glue
function", found: "a generic or member function".to_string() }`. `found` is
documented as "built directly from a `TokenKind`"; here it is prose. The
error deserves its own variant.

### What changes

| Crate | Area | Change |
|---|---|---|
| `omega-parser` | `ast/**` | 48 files → 6; deep paths retired in favour of `prelude` |
| `omega-parser` | `lexer.rs` | one token table drives lexing *and* `describe()` |
| `omega-parser` | `parser/**` | contextual-keyword registry; precedence table; 9 duplicated loops → 2 helpers |
| `omega-parser` | `diagnostics.rs` | one definition site per error |
| `omega-parser` | AST nodes | real spans on functions, params/fields, spec functions |
| `omega-parser` | `macros.rs` | context struct; in-place traversal |
| `omega-diagnostics` | `diagnostic.rs` | ordered footers |
| `omega-diagnostics` | `render.rs` | shared label geometry; typed elision marker |
| `omega-hir` | `hir.rs` | `HirRangeEnd`; block spans; global-vs-local declaration split |
| `omega-hir` | `lower.rs` | node-construction helper; consistent id order |
| `omega-hir` | `lib.rs` | the crate doc it does not have |
| `omega-hir` | tests | first tests for this crate (currently zero) |
| `omega-analyzer` | anchor sites | consume the new spans (compatibility patch only) |
| `docs/` | new `15-parsing-and-hir.md`, `14-known-issues.md` | the deferred language questions |

### What must not change

- **The accepted language.** No program that compiles today may stop
  compiling, and none that is rejected today may start compiling. The two
  new diagnostics (E.7, E.8) change *messages*, never verdicts.
- **`Span` stays file-less.** Its doc comment gives the reason; nothing here
  disturbs it.
- **The macro-expansion architecture.** Parse → find invocations in the tree
  → splice tokens → re-parse at the right entry point, with call-site span
  re-anchoring. `macros.rs`'s top doc comment explains why; only the
  boilerplate around it changes.
- **`struct`/`enum`/`union` as three item pipelines.** See resolved concern 1.
- **`omega-hir` as a separate crate and a separate tree.** See resolved
  concern 2.
- **`omega-diagnostics` depending on nothing.** It is the graph's root; the
  `Highlighter` trait exists precisely so the arrow stays parser → diagnostics.
- **Backtracking discipline.** `mark`/`reset` sites (`parse_codeblock`'s
  tail-vs-statement, `try_parse_generic_args`,
  `recover_restricted_struct_literal`) are load-bearing and subtle. Do not
  "simplify" them.
- **Formatting of files this refactor does not otherwise touch.** Do **not**
  run `cargo fmt --all` (or `cargo fmt` outside a file you are already
  editing). There is no `rustfmt.toml`; the workspace has never been
  rustfmt-managed, and 719 lines at `HEAD` exceed rustfmt's default 100-column
  width. A blanket format reflows the entire workspace — ~3,000 lines across
  crates unrelated to this plan — which buries the refactor and defeats every
  per-phase diff-scope checkpoint below. Adopting a repo-wide format is a
  reasonable thing to do; it is a separate change, on its own commit, decided
  separately.

### Chosen approach

Nine phases, each independently buildable and testable. **Phase A is
deliberately first**: it is a pure file move with zero logic change, and
doing it before the content edits means no later phase has to be re-applied
across a moved layout. The two highest-risk phases (F, spans, which crosses
into `omega-analyzer`; G, macro traversal) come after the mechanical ones, so
that if the pass is cut short the cheap wins are already banked.

### Risks and open questions

- **Phase A touches ~35 external import sites.** Purely mechanical, but if
  `cargo check` after A shows anything beyond import-path changes, stop.
- **Phase F changes an `omega-analyzer` public-ish surface** (`HirFunctionDef`
  gains fields). Additive; no analyzer logic changes except which span is
  handed to two error constructors.
- **Phase G's in-place traversal** must preserve the exact call-site
  re-anchoring behaviour. `compiler/omega-parser/tests/macros.rs` (10 tests)
  is the guard; if any of them needs its *expectations* edited rather than
  just its call syntax, stop and ask.
- **Contextual-keyword registry (Phase D)** must not accidentally reserve
  anything. The regression guard is an explicit test that every registered
  word still parses as an ordinary identifier.

---

## Implementation Plan

> **Status: complete.** Every phase (A–I) is implemented and verified —
> 274 workspace tests, all ten `just test-*` gates, `just run-exec` (exit
> 69), and `target/core.o` byte-identical to `HEAD`'s, which is the plan's
> own hardest gate: no phase here may change generated code.
>
> Beyond the plan, and recorded rather than smuggled in:
>
> - **`!`, `&&` and `||`** were added at the user's request. I.2 had listed
>   "no logical negation" as a question to *record*, not decide; the user
>   decided it. All three desugar during analysis (`!x` to `x ^ true`,
>   `&&`/`||` to the `if`-expressions the idiom already used), so no
>   `CheckedExpr`, MIR or codegen variant was needed.
> - **Four contextual-keyword commit-rule fixes** that *widen* the accepted
>   grammar: `reveal`/`comp` could be declared but never read, `exposed`/
>   `internal` could not name a field or binding, and `comp <i32>5` briefly
>   regressed during the work and was repaired.
> - **One accepted-language break**: `a&&b` written without spaces used to
>   mean `a & (&b)`. Zero occurrences in `runtime/` or `examples/`; C has
>   the same ambiguity. Recorded in `docs/14-known-issues.md` as a decision,
>   not patched over.
>
> Deferred deliberately, each with an entry in `docs/14-known-issues.md`:
> the `HirParam`/`CheckedParam`/`(Ident, ResolvedType, Visibility)`
> conflation (fix as one unit in the analyzer pass — splitting `HirParam`
> alone creates a distinction that dies one layer later), `CheckedSlice`'s
> flattened range end, three diagnostics still anchored wider than their
> subject, and the synthesized-`HirId` invariant.

### Phase A — AST layout and public surface

**A.1** Collapse `compiler/omega-parser/src/ast/expression/` (27 files,
~450 lines total, average 17) into a single `ast/expression.rs`. Keep the
existing `Expression` enum and `ExpressionNode` at the top of the file, then
the per-variant structs in the same order as the enum's variants. Every doc
comment moves verbatim. Public paths become `ast::expression::IfExpr` instead
of `ast::expression::if_expr::IfExpr`.

**A.2** Collapse `ast/statement/` (21 files) into **two** files, splitting on
the real distinction the module already draws in its own comments:
`ast/item.rs` (`Item`, `ItemNode`, and the item-only statement structs:
`StructStmt`, `UnionStmt`, `EnumStmt` + `EnumHeaderField`/`EnumVariantStmt`,
`SpecStmt` + `SpecFunctionStmt`, `GapStmt`, `GlueStmt`, `ConformStmt`,
`PrimitiveStmt`, `ImportStmt` + `ImportRoot`, `MacroDefinitionStmt` and its
supporting types) and `ast/statement.rs` (`Statement`, `StatementNode`, and
the body-scope structs: `DeclarationStmt`, `ExternDeclarationStmt`,
`ReturnStmt`, `WalrusStmt`, `WhileStmt`, `LoopStmt`, `ForStmt`, `ForInStmt`,
`DeferStmt`, `FunctionDefinitionStmt`).

**A.3** Delete the "kept in the same file-per-construct layout as before for
continuity" note in `ast/mod.rs` — the layout it referred to is gone. Replace
it with a one-line statement of the new rule: one file per *tier* of the
grammar, not one per node.

**A.4** Update `prelude.rs` for the new paths. Its export list stays exactly
the same set of names.

**A.5** Rewrite the ~35 deep imports outside `omega-parser`
(`omega_parser::ast::…`) to go through `omega_parser::prelude`. Add to
`lib.rs`'s crate doc: *`prelude` is this crate's supported surface; the
module layout under `ast`/`parser` is an implementation detail.* This is the
modularity point — today the parser's file layout is part of its API.

**A.6** Move `#[cfg(test)] mod tests` in `parser/expression.rs` (currently
sitting between `parse_pattern` and `parse_codeblock`) to the end of the file.

*Checkpoint: `cargo test --workspace` green; `git diff --stat` should show
moves and import rewrites only.*

**Establishing the baseline for this and every later checkpoint.** Record
`git status --short` *before* starting a phase; that is the baseline. If a
checkpoint's diff looks wider than expected, do not guess and do not revert
blindly — classify each changed file first. A file is provably
formatting-only when rustfmt applied to its `HEAD` version is byte-identical
to the working copy:

```sh
git show "HEAD:$f" > /tmp/head.rs && rustfmt --edition 2024 --quiet /tmp/head.rs \
  && cmp -s /tmp/head.rs "$f" && echo "formatting-only: $f"
```

Reverting a file that passes that test loses nothing. Anything that fails it
is a real edit and must be reviewed, not discarded. If the classification
still leaves changes you cannot account for, stop and ask — that is the
correct call.

### Phase B — `omega-diagnostics`

This crate is in good shape; the work here is small and deliberate.

**B.1** Replace `Diagnostic`'s twin `notes: Vec<String>` / `helps:
Vec<String>` with one ordered `footers: Vec<Footer>`, where
`enum Footer { Note(String), Help(String) }`. Today a site that writes
`.with_help(a).with_note(b)` renders note-then-help, silently reordering the
author's intent. `with_note`/`with_help` stay as the constructors; only
`Renderer::render`'s two loops become one.

**B.2** Extract the caret-column arithmetic shared by
`render_single_underline` and `render_multiline_label` — both compute
`span → line_start → byte offset → display_col` with the same
`saturating_sub`/`min` clamping — into one `fn label_columns(file, span,
line) -> (usize, usize)`. Roughly 15 lines of near-duplicate index math with
two different off-by-one adjustments folded into one place.

**B.3** Replace the `line == 0` in-band elision sentinel in
`render_multiline_label`'s `body` vector with an explicit
`enum BodyRow { Source(usize), Elision }`. `0` is not a line number and the
reader has to prove that to themselves.

**B.4** Add renderer tests for two shapes not currently covered: a
`Footer`-ordering case (help before note stays help before note), and two
labels on the *same* line (must print the source line once with two
underline rows).

### Phase C — the lexer's token table

**C.1** Add `TokenKind::spelling(&self) -> Option<&'static str>`: the exact
source text of every fixed token (all keywords, all punctuation), `None` for
the six payload-bearing variants and `Eof`.

**C.2** Rewrite `describe()` in terms of it — payload variants keep their
`format!`, everything else becomes `format!("'{}'", self.spelling()?)`. This
deletes ~70 hand-written `.to_string()` arms whose only content is a spelling
already listed elsewhere in the file.

**C.3** Derive `KEYWORDS` and `MULTI_CHAR_PUNCT` from one source. Both
tables, plus `scan_punct`'s single-char match, plus `describe()` currently
spell the same strings four times.

**C.4** Remove the maximal-munch ordering hazard. `MULTI_CHAR_PUNCT` is
first-match-wins by *list order*, guarded only by two prose comments warning
future editors to keep three-char forms above their two-char prefixes. Either
sort the table by descending length at the match site, or add a test that
asserts the table is sorted that way. Prefer the former: it makes the hazard
unrepresentable rather than merely detected.

**C.5** `scan_ident`'s keyword lookup becomes a `match text` instead of a
linear scan over the table, and `scan_metavar` returns `TokenKind` rather
than `Result<TokenKind, ParseError>` — it has no error path.

### Phase D — the contextual-keyword registry

Omega deliberately reserves as few words as it can, recognising 17 of them by
position: `mut`, `comp`, `self`, `reveal`, `sizeof`, `in`, `exposed`,
`internal`, `marker`, `gap`, `glue`, `conform`, `to`, `primitive`, `root`,
and the macro fragment kinds `expr`/`type`/`ident`. Today that set exists
only as 33 scattered `matches!(p.peek(), TokenKind::Ident(name) if name ==
"…")` literals across five files. Nothing states the set; nothing prevents a
typo; nothing tells a language designer what is already spoken for.

**D.1** Add `compiler/omega-parser/src/parser/contextual.rs`: one `const`
table of every contextual keyword, each with a one-line doc saying *in which
position* it is a keyword and what it means everywhere else.

**D.2** Add three `Parser` methods — `at_contextual(&self, kw) -> bool`,
`at_contextual_at(&self, n, kw) -> bool`, `eat_contextual(&mut self, kw) ->
bool` — and replace all 33 sites. Two immediate readability wins: the 30-line
`mut`/`comp` offset arithmetic duplicated between `parser/item.rs:57` and
`parser/statement.rs:79` becomes legible, and `parse_conform_def`'s
`p.expect(&TokenKind::Ident("to".to_string()), "'to'")` — which allocates a
`String` to compare against — becomes `p.expect_contextual(TO)`.

**D.3** Add a regression test asserting every word in the table still parses
as an ordinary identifier in ordinary positions (binding name, function name,
call target). Several such tests exist ad hoc today
(`gap_and_glue_stay_ordinary_identifiers`,
`conform_to_and_primitive_stay_contextual_identifiers`); this generalises
them so a newly-added contextual keyword cannot quietly reserve a word.

### Phase E — grammar de-duplication and error definitions

**E.1** Replace the six left-associative tier functions in
`parser/expression.rs` (`parse_bitor`, `parse_bitxor`, `parse_bitand`,
`parse_shift`, `parse_additive`, `parse_multiplicative` — lines 118–219,
each the same 12-line loop) with one `parse_binary_tier(p, tier)` driven by a
`const BINARY_TIERS: &[&[(TokenKind, BinaryOp)]]` listed loosest-to-tightest.
`parse_expression`'s doc comment already narrates that ordering in prose;
this makes the prose executable. `parse_assignment` (right-associative) and
`parse_comparison` (non-associative) stay explicit outer layers — that
asymmetry is real and the doc comment already justifies it.

**E.2** Extract `parse_aggregate_fields(p) -> Vec<DeclarationStmt>` — the
`while field_follows(p) { visibility; parse_declaration; expect ';'; recover }`
loop, currently copied verbatim four times: `parse_struct_or_marker_body`
(item.rs:644), `parse_union_def` (:710), `parse_enum_def`'s dynamic-fields
section (:996), `parse_enum_variant`'s body (:1127).

**E.3** Extract `parse_member_functions(p, policy)` — the
`while Ident|At { annotations; visibility; parse_function_definition; recover }`
loop, currently copied five times (`parse_struct_or_marker_body`,
`parse_union_def`, `parse_enum_def`, `parse_conform_def`,
`parse_primitive_def`). `policy` is a small struct saying whether a member
visibility modifier is accepted, rejected with `ConformMethodVisibility`, or
ignored. **This is also where D5 gets fixed:** all five (plus `gap`/`glue`)
recover per-member instead of four recovering and four aborting.

**E.4** Merge the two parameter-list parsers. `parser/item.rs:572`
`parse_param_list` → `Vec<DeclarationStmt>` and `parser/type.rs:154`
`parse_param_list` → `Vec<(Ident, Type)>` are the same production written
twice, and their `parse_declaration_list`/`parse_decl_list` bodies (item.rs
:591, type.rs :169) are character-for-character identical apart from the
element type. Introduce one `ast::Param { ident, r#type, span }`, use it for
both, and change `FunctionType::params` to `Vec<Param>`.

  - `FunctionType` derives `PartialEq`/`Eq`, so `Param` must compare on
    `ident`/`r#type` only. There is direct precedent: `Path` hand-writes
    `PartialEq`/`Hash` to exclude `origin`, with the comment "Origin is
    resolution provenance, not syntax." A span is the same kind of thing.
  - Bonus: `HirFunctionDef::function_type()` currently rebuilds a
    `Vec<(Ident, Type)>` by cloning every param; it becomes a direct map.

**E.5** Extract the `[mut] [comp] ident (':='|':')` lookahead — 30 lines
duplicated between `parser/item.rs:57` and `parser/statement.rs:79`,
including near-identical 15-line comments — into one
`parse_binding_prefix(p) -> Option<BindingPrefix>`. Their two follow-on
functions (`parse_item_declaration_or_walrus`,
`parse_walrus_or_declaration`) differ only in whether they wrap in `Item` or
`Statement`; keep both but have them share the `= value` tail.

**E.6** Collapse the three-way split of every parse error's definition.
Today adding one error means editing three exhaustive matches in three
different orders — the `ParseErrorKind` variant (with its doc comment),
`Display` (the headline), and `to_diagnostic` (labels/notes/helps). Make
`to_diagnostic` the single site that knows an error's text, and define
`Display` as its `message`. While there, fix the stale doc reference on
`GapFunctionBody` — it points at `ParseError::render`, a method that does not
exist (it is `to_diagnostic`).

**E.7** Add `ParseErrorKind::GlueFunctionShape { name }`, replacing D6's
`Expected` abuse in `parse_glue_def`.

**E.8** Add `ParseErrorKind::ChainedComparison` for D4, reported by
`parse_comparison` when a second comparison operator follows the first, with
a help suggesting parentheses. This is diagnosis only — the input is rejected
today too, just unintelligibly.

### Phase F — real spans (fixes D1 and D2)

The root cause is that a span is carried by the wrapper node
(`ItemNode`/`StatementNode`/`ExpressionNode`) rather than by the construct,
so anything never wrapped in one — every struct/union/enum member, every
field, every parameter, every spec function — inherits its parent's span.

**F.1** `FunctionDefinitionStmt` gains `name_span`, `signature_span` (name
through return type, excluding the body) and `return_type_span`. Set in
`parse_function_definition`, which already has every needed `p.last_span()`.

**F.2** `DeclarationStmt` gains `span` and `name_span`. Set in
`parse_declaration`. `ast::Param` (E.4) already carries one.

**F.3** `SpecFunctionStmt` gains the same three spans as F.1, set in
`parse_spec_function` (shared by `spec` and `gap` bodies).

**F.4** `omega-hir`: `HirFunctionDef` and `HirSpecFunction` gain
`signature_span`/`return_type_span`; `HirParam::span` becomes the real one.
Delete the "an approximation but strictly better than nothing" note in
`lower_function_def` and the matching one in `lower_enum_def` — they stop
being true.

**F.5** `omega-analyzer` (compatibility patch, no logic change): anchor
`ReturnTypeMismatch` at `return_type_span`
(`analysis/items.rs:530` `check_return_type`, and the `return` statement site
at `analysis/stmts.rs:364`), and `Redeclaration` at the declaration's
`name_span`. Confirm with the D1/D2 reproductions above.

**F.6** `HirBlock` gains a span. It currently has none, so a block-level
diagnostic has nowhere to anchor; the parser has the braces right there.

### Phase G — `macros.rs`

**G.1** Introduce `struct Expander<'a> { defs: &'a HashMap<Ident,
MacroDefinitionStmt>, budget: u32, state: &'a mut ExpansionState }` and make
the fifteen `expand_*` free functions its methods. The `(defs, budget,
state)` triple is threaded through every one of them by hand today; the
signatures are longer than several of the bodies.

**G.2** Convert the expansion traversal from rebuild-by-value to in-place
`&mut`. `expand_expr` (macros.rs:1082) is ~180 lines of which the substantive
part is the first eight — find `Expression::MacroInvocation`, replace it. The
rest reconstructs every AST node field-by-field purely to recurse. With
`fn children_mut(&mut self) -> impl Iterator<Item = &mut ExpressionNode>` on
`Expression` (one match, one line per variant), the traversal becomes:
find-and-replace at this node, else recurse into children.

  - Block-bearing variants (`Codeblock`, `If`, `Match`, `Slice`'s range)
    contain statements, not just expressions; they keep explicit arms. Keep
    the arm count honest — the point is deleting reconstruction, not hiding
    structure.
  - `children_mut` is written once and is the piece a future pass can reuse.
    Do **not** build a general `Fold`/`Visit` trait for it: there is exactly
    one consumer today, and `omega_hir::lower` cannot share it (it produces a
    different tree).

**G.3** `expand_struct_def`/`expand_union_def` are identical modulo type;
`expand_enum_def` differs only by also walking variant args. Fold the shared
part into one helper following E.3's precedent.

### Phase H — `omega-hir`

**H.1** Add `Lowerer::node(&mut self, span: Span, expr: HirExpr) ->
HirExprNode`. `lower_expr` (lower.rs:691) writes `HirExprNode { id:
self.ids.next(), span: node.span, expr: … }` twenty-odd times. This also
fixes a real inconsistency: because Rust evaluates struct fields in source
order, arms that bind children in a preceding `let` mint the parent's id
*after* its children, while arms that inline mint it *before*. Nothing
depends on the order today, which is exactly why it should be made uniform
now rather than after something does.

**H.2** Fix D3: give `HirRange` a `HirRangeEnd { Inclusive(Box<HirExprNode>),
Exclusive(Box<HirExprNode>), Open }` mirroring `ast::range::RangeEnd`,
replacing `end: Option<Box<HirExprNode>> + inclusive: bool`. `lower_range`
stops flattening. Update the six consumer sites in `omega-analyzer`
(`analysis/patterns.rs` ×5, `analysis/places.rs:685`,
`analysis/exprs.rs:1856`); `checked.rs:708`'s comment that
`CheckedRange` "mirrors `omega_hir::HirRange`'s" shape should be re-checked
and either updated or the same fix applied there — flag it, do not silently
extend scope.

**H.3** Make global-vs-local visibility structurally meaningful.
`HirDeclaration::visibility` and `HirWalrusDeclaration::visibility` are
documented as "meaningful for a top-level global, left at its default for a
local statement declaration, which never has one" — a doc comment standing in
for a type. Introduce `HirGlobal { decl: HirDeclaration, visibility:
Visibility }` (and the walrus equivalent) used by `HirItem::Declaration`/
`DeclarationWithInit`/`Walrus`, and drop the field from the statement-level
node.

Blast radius is four lines: `omega-analyzer/src/analysis/mod.rs:229-232`
reads `.visibility` uniformly off every `HirItem` variant and is the entire
consumer surface, plus the construction sites in `lower.rs`.

**H.3 explicitly does NOT touch `HirParam::visibility`.** `HirParam` serves
two roles — a function/spec parameter, where visibility is meaningless, and
an aggregate field (`HirStructDef::fields`, `HirUnionDef::fields`, enum
header/dynamic/variant fields), where it is the *sole* representation of
field visibility. Splitting it looks like the same fix, but it is not
self-contained the way H.3 is:

- The identical conflation exists one layer down in `omega-analyzer`.
  `CheckedParam { id, span, ident, r#type }` (`checked.rs:160`) carries no
  visibility at all and is used for **both** `CheckedFunctionDef::params`
  **and** `CheckedStructDef`/`CheckedUnionDef::fields`.
- Field visibility bypasses `CheckedParam` entirely: it is read at four sites
  in `analysis/items.rs` (:879, :937, :1005, :1179) and lands in
  `ResolvedStructType`/`ResolvedUnionType`/`ResolvedEnumType::fields:
  Vec<(Ident, ResolvedType, Visibility)>` — an untyped triple that is its own
  piece of debt.

Introducing `HirField` here would create a type distinction that dies one
crate later: HIR would separate fields from parameters, and
`analyze_struct_fields` would immediately re-merge them into `CheckedParam`
while shipping visibility out a side channel. That is worse than either
endpoint, and `omega-analyzer` is out of scope for this step by instruction
(compatibility patches only; its own refactor plan comes later). The whole
three-layer chain — `HirParam` → `CheckedParam` → the `(Ident, ResolvedType,
Visibility)` triple — should be fixed as **one unit** in the
`omega-analyzer` pass. Recorded in I.2 so it is not lost.

**H.4** Write `omega-hir/src/lib.rs`'s crate doc. It is seven lines of `pub
mod`/`pub use` with no prose at all, for the crate whose *reason to exist* is
the least obvious in the workspace. State: what HIR is, why it is a separate
tree from the AST (resolved concern 2), the four desugarings it owns, what it
deliberately does *not* do (no name resolution, no type checking, infallible),
and where ids come from.

**H.5** Add the crate's first tests — it has **zero** today, against 34 in
`omega-parser` and 10 in `omega-diagnostics`. Minimum set in *Testing* below.

### Phase I — documentation

**I.1** New `docs/15-parsing-and-hir.md` (the slot is free, and it sits
directly before `16-mir-and-codegen.md`, which documents the pipeline's back
half in exactly this style). Contents: the lexer/parser/expansion/lowering
pipeline; the one-fact-one-home table from this plan's *Reasoning*; the
contextual-keyword registry and the reserve-as-little-as-possible policy; why
HIR exists; the span-ownership rule established in Phase F (*a construct that
can be the subject of a diagnostic owns its span*); and a Caveats section
like every other doc file.

**I.2** Add to `docs/14-known-issues.md` the language-level questions this
pass surfaced but must not decide (goal 7). Each is a *design* question, not
a bug:

  - **No logical negation.** `!` is lexable only as part of `!=`; `if !done
    { }` fails with "unexpected character '!'". Combined with the deliberate
    absence of `&&`/`||` (docs/03), boolean negation has no spelling at all.
    Is that intended? → **Control flow** section.
  - **17 contextual keywords and no policy.** The count grows with every
    feature; each one is a position-dependent ambiguity. Worth a stated rule
    for when a word graduates to a real keyword. → **Design debt worth
    watching**.
  - **Chained comparison is permanently a syntax error.** E.8 makes the
    message honest; whether `a < b < c` should ever mean something (Python
    chains it, Rust rejects it) is a language decision. → **Control flow**.
  - **Diagnostics have no error codes and no machine-applicable
    suggestions.** `Diagnostic` carries a message, labels and footers only.
    Both are additive later, but the sooner the shape is decided the fewer
    sites need revisiting. → **Diagnostics**.
  - **`Type`'s derived `PartialEq` compares parameter *names*** inside
    `FunctionType`, so `(a: i32) => void` ≠ `(b: i32) => void` structurally.
    Harmless today (the analyzer compares `ResolvedType`), latent if raw
    `Type` equality ever becomes load-bearing. → **Types**.
  - **A dangling annotation at EOF is silently dropped**: `@inline` with no
    item after it reports only "expected a top-level item, found end of
    input" and the annotation vanishes. → **Diagnostics**.

  Plus one cross-crate item that is *not* a language question but must not be
  lost between refactor steps (see H.3 for why it is deferred rather than
  done):

  - **Parameters and aggregate fields are the same type at all three
    layers.** `omega_hir::HirParam` carries a `visibility` meaningful only in
    the field role; `omega_analyzer::CheckedParam` serves both roles and
    carries no visibility at all; field visibility travels separately as
    `Vec<(Ident, ResolvedType, Visibility)>` on
    `ResolvedStructType`/`ResolvedUnionType`/`ResolvedEnumType`. Three
    representations of one fact, spread across two crates. Fix as one unit in
    the `omega-analyzer` pass — splitting `HirParam` alone would create a
    distinction that dies at the next layer. → **Design debt worth
    watching**.

**I.3** Update `docs/README.md`'s reading order to include `15-…`, and
refresh the stale line in its header describing the compiler as
"Cranelift backend" now that both backends ship.

**I.4** Fix the stale comment in `parser/expression.rs::parse_primary`: it
justifies trying macro invocation before `Path` because "an identifier
immediately followed by `!`" — the macro sigil has been `$` for some time.
(This is also the likely reason `!` has no token: it was freed and never
reassigned. Relevant to I.2's first bullet.)

---

## Testing

### New cases, per phase

- **A** — none; correctness is `cargo test --workspace` staying green with a
  diff that contains only moves and import rewrites.
- **B** — footer ordering preserved (help-then-note renders in that order);
  two labels on one source line print the line once with two underline rows.
- **C** — `spelling()` round-trips: for every fixed `TokenKind`, lexing its
  spelling yields it back. This is the test that makes C.3/C.4 safe, and it
  catches a mis-ordered munch table directly (`<<=` must not lex as `<` `<=`).
- **D** — every registered contextual keyword parses as an ordinary
  identifier in binding, function-name and call position (generalises the two
  existing ad-hoc tests).
- **E** — precedence unchanged across the rewritten tiers: assert the tree
  shape for `a | b ^ c & d << e + f * g`, and that `a & b == c` parses as
  `(a & b) == c` (the C footgun the doc comment says Omega avoids);
  per-member recovery in `conform`/`primitive`/`gap`/`glue` bodies reports
  one error and still parses subsequent members.
- **F** — the D1 and D2 reproductions become assertions: a duplicate field's
  primary label covers the field name only; a return-type mismatch's label
  covers the declared return type only. Add the method-level variants of both.
- **G** — the existing 10 macro tests must pass with expectations untouched;
  add one nested-in-block-expression expansion case
  (`if cond { m$(x) } else { m$(y) }`) to cover the block-bearing arms.
- **H** — `omega-hir`'s first tests: `self` parameter inserted with the right
  shape per `SelfMode`; `mut self` produces the shadow walrus as statement 0;
  `f(x: spec Foo, y: *spec Bar)` yields two fresh `$ParamN` generics with the
  right bounds; `a.b[i].c` flattens to one `HirPlace` with three projections
  in source order; `foo().bar` roots at `HirPlaceRoot::Expr`; every `HirId` in
  a lowered module is unique; `HirRangeEnd` round-trips all three spellings.

### Negative cases

- `a < b < c` → `ChainedComparison`, not "expected ';'" (E.8).
- A `glue` body containing a generic or `self`-taking function →
  `GlueFunctionShape`, not `Expected` (E.7).
- Malformed member inside `conform`/`primitive` → exactly one error, and the
  members after it still parse (E.3/D5).
- Each phase's exhaustive-match removals must not lose a rejection: after E.6,
  every `ParseErrorKind` still produces both a headline and at least one
  label. Add a test that constructs one of each variant and asserts
  `to_diagnostic()` has a non-empty message and at least one label — this
  replaces the compiler's exhaustiveness check that E.6 removes.

### Regression risk

Most likely to break, in order:

1. **`parser/expression.rs`'s speculative parses** (E.1 sits next to them).
   `generic_args_do_not_steal_comparisons` and
   `unambiguous_literal_in_condition_reports_dedicated_error` are the canaries.
2. **`omega-analyzer` after F.5** — full `cargo test --workspace`, then the
   `just` end-to-end gates.
3. **Macro expansion after G.2** — `compiler/omega-parser/tests/macros.rs`.
4. **Everything downstream of H.2/H.3** — `HirRange` and the visibility split
   are the only HIR shape changes with real consumers.

### Full-pipeline verification

The unit tests cannot catch a lowering regression on their own. After each of
Phases F, G and H, run the end-to-end gates that already exist:

```
cargo test --workspace
just test-core-only test-range test-char test-spec-dispatch test-spec-calls \
         test-root-layout test-allocator-only test-io test-stdio-contract \
         test-multi-print
just run-exec          # must still exit 69
```

Baseline to preserve: 70 tests across the three crates today
(`omega-diagnostics` 10 + 1 doctest, `omega-parser` 34 + 9 + 10 + 6,
`omega-hir` 0).

**Byte-identical output check.** No phase in this plan should change a single
byte of generated code. After Phase H, rebuild `target/core.o` and compare it
against a copy taken from `HEAD` before the refactor began. Any difference is
a bug in this refactor, not an improvement — investigate before proceeding.

### Target coverage

Not applicable — nothing here is target-dependent. The `just test-*-llvm`
gates need only be run once at the end as a smoke check, since the front end
is shared by both backends.
