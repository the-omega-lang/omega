# Divergence coherence: `never` in value positions, assignment sequencing, bare `return;`

## Task Description

- **Deliverable:** a diverging expression is accepted in every expected-value
  position the specification says it is; an `if` whose arms all diverge is
  typed `never` rather than `void`; `never` stops leaking into inferred local
  storage; an assignment target's dynamic components are evaluated before the
  right-hand side can divert control; and `return;` parses in a `void`
  function. Four entries leave `docs/issues/known-issues.md`; two newly found
  defects are fixed with them and one new entry is added for what stays out.

- **Purpose:** `docs/language/types-and-primitives.md#never` states the rule —
  *"A diverging expression is compatible with any expected expression type"* —
  and the compiler honours it in only two of ten positions. These are not ten
  bugs: they are one missing rule reached through ten call sites, plus one
  place where the compiler computes divergence correctly and then discards the
  answer. Fixing them together is what removes the rule's duplication rather
  than adding a tenth special case.

- **Chosen direction:**
  1. **One value-position predicate.** Add a single analyzer helper meaning
     "may this checked expression inhabit this expected type", defined as
     `expected.accepts(found) || *found == ResolvedType::Never`, and route
     every value site through it. It does **not** go into
     `ResolvedType::accepts`: `accepts` also serves generic type-pattern
     matching (`generics/pattern.rs`), conformance signature checking
     (`analysis/items/mod.rs:564`) and overload pattern matching
     (`calls/overload.rs:504`), where "a `never` return satisfies an `i32`
     requirement" would be wrong. It also does not go into `plan_coercion`:
     divergence is not a conversion and must leave no `CheckedCoercionStep`.
  2. **One representation of divergence.** The analyzer currently carries two:
     `ResolvedType::Never` on a node's type, and the structural
     `block_type() == None` / `expr_diverges` analysis. `analyze_if` computes
     the structural answer and then throws it away, typing an all-arms-diverge
     `if` as `Void` (`exprs/mod.rs:388-398`) — which is why
     `consume(if f { exit(1) } else { exit(2) })` reports *"found 'void'"*.
     Make `analyze_if` collapse to `Never` exactly as the codeblock path
     already does at `exprs/mod.rs:50`
     (`block_type(&checked).unwrap_or(ResolvedType::Never)`), then delete the
     now-redundant `If` arm of `expr_diverges` (`stmts.rs:14-23`), whose result
     the `r#type == Never` test at `stmts.rs:10` subsumes. This removes a
     duplicate mechanism rather than teaching the new predicate about the
     second one.
  3. **Reuse the existing MIR sequencing mechanism.** `lower_place_evaluated_once`
     (`omega-mir/src/lower/place.rs:25`) already forces a place's dynamic
     components into locals, and compound assignment already uses it for the
     same reason. Ordinary assignment adopts it; nothing new is invented.
  4. **Desugar bare `return;` at HIR lowering.** The AST field becomes
     `Option`, and HIR lowering synthesizes a void expression for `None`.
     Analyzer, checked tree, MIR and codegen are untouched, and `return;` in a
     non-`void` function falls out as an ordinary `ReturnTypeMismatch`.

- **Runtime checking — answer to "can we panic after `never` too":** no new
  check, and this is a deliberate conclusion rather than a deferral. The panic
  machinery already exists (`RuntimeCheck::NeverCallReturned`,
  `omega-analyzer/src/runtime_checks.rs:27`; `guard_never_call`,
  `omega-mir/src/lower/function/expr.rs:248`) and is already applied at the
  only boundary where it can catch anything: a `never` that is a **contract
  with code the compiler does not control** — a `foreign` declaration, a gap
  function, a function pointer, a spec's dynamic call. Compiler-proved
  divergence (`loop` with no reachable `break`, a diverging block, an `if`
  whose arms all diverge, `return`/`break`/`continue`) has no return edge to
  check; emitting a panic there would be unreachable code paid for in every
  binary, which the "no hidden cost" invariant forbids. The specification
  already fixes this policy — *"The check is on the call, not on every
  expression whose type is `never`"* — so widening it would be a language
  change, not a bug fix. **What this task owes the question is proof, not new
  machinery:** the positions being unlocked here are exactly the positions the
  guard has never been exercised in, so the t44 family gains direct
  argument-position and field-position cases (step 10). Do not change
  `unreachable_value` (`omega-mir/src/lower/function.rs:293`) to emit a trap
  either: it returns a read of a fresh local that is dead by construction
  because the block is already terminated, and step 7 proves that property
  instead of paying for it at runtime.

- **Rejected alternatives:**
  - *Widening `ResolvedType::accepts`* — leaks "never is acceptable" into
    signature, pattern and overload matching (see above).
  - *Special-casing `never` at each failing call site* — that is the shape the
    bug already has; it is why the same defect appears in ten places.
  - *Threading `Option<HirExprNode>` through HIR/Checked/MIR for `return;`* —
    five crates changed to express something the void type already expresses.
    Prefer it only if step 9 finds the synthesized node produces a wrong span
    in a diagnostic; escalate rather than switching silently.

## Technical Details

- **Initial context boundary:** `compiler/omega-analyzer/src/analysis/`
  (`mod.rs`, `stmts.rs`, `literals.rs`, `exprs/mod.rs`, `exprs/operators.rs`,
  `calls/`), `compiler/omega-mir/src/lower/` (`function.rs`,
  `function/expr.rs`, `place.rs`), the single `return` site in
  `omega-parser`/`omega-hir`, plus
  `docs/language/types-and-primitives.md#never` and
  `docs/language/grammar.md:428`. Open `docs/architecture/semantic-analysis.md`
  only if the predicate's placement needs justifying beyond this plan;
  `omega-codegen` stays closed.

- **Affected files/symbols:**

  *Analyzer — the value sites that must adopt the shared predicate.* These
  were confirmed by compiling probe programs against the current `omgc`, not
  inferred; each currently rejects a `never` operand:

  | Site | Position | Current diagnostic |
  |---|---|---|
  | `analysis/calls/mod.rs:925` | call argument | `ArgumentTypeMismatch` |
  | `analysis/calls/generic.rs:39` | generic call argument | `ArgumentTypeMismatch` |
  | `analysis/calls/spec.rs:240,485,613` | spec/dynamic call arguments | `ArgumentTypeMismatch` |
  | `analysis/exprs/mod.rs:506` | second call-argument path | `ArgumentTypeMismatch` |
  | `analysis/literals.rs:243,335` | struct/union literal field | `FieldTypeMismatch` |
  | `analysis/literals.rs:1038` | array literal element | mismatched types in array literal |
  | `analysis/exprs/mod.rs:574` | assignment value | `AssignmentTypeMismatch` |
  | `analysis/items/mod.rs:216` | annotated declaration initializer | `AssignmentTypeMismatch` |
  | `analysis/stmts.rs:281` | `return <expr>;` | `ReturnTypeMismatch` |
  | `analysis/exprs/operators.rs:96` | `&&`/`\|\|` operand | `InvalidLogicalOperand` |

  Every one of these already calls `coerce_to_expected` first and then re-tests
  with `accepts`; `plan_coercion` correctly returns `None` for a `never` value,
  so the node arrives at the test still typed `Never`. Only the test changes.

  *Analyzer — other sites.* `analyze_if` result type
  (`exprs/mod.rs:388-398`); `expr_diverges`'s `If` arm (`stmts.rs:14-23`);
  `Analyzer::conversion_cost` (`mod.rs:1011`), which documents that overload
  viability "can never disagree with the coercion that actually runs" and so
  must accept `never` at cost `0` once arguments do; `analyze_walrus`
  (`stmts.rs:173-204`), which adopts the initializer's type with no
  storable-type gate.

  *Analyzer — sites that must NOT change:* `generics/pattern.rs:164,170,243,247`,
  `calls/overload.rs:504`, `items/mod.rs:564` (`check_function_return`),
  `patterns.rs:1209` and the `mismatch` scan in `analyze_if`
  (`exprs/mod.rs:405`) — the last two already skip diverging arms correctly by
  filtering `None`.

  *MIR:* `lower_assignment_stmt` (`lower/function.rs:536-568`) switches
  `lower_place` for `lower_place_evaluated_once`. Audit the other dynamic
  place/bounds lowering the issue names as related candidates; `lower_sequence`
  (`lower/function/expr.rs:324`) already provides the guarantee for call,
  binary and aggregate operands and should not be duplicated.

  *`return;`:* `omega-parser/src/ast/statement.rs:48` (field becomes
  `Option<ExpressionNode>`), `omega-parser/src/parser/statement.rs:214`,
  `omega-parser/src/macros/expander.rs:504`,
  `omega-hir/src/lower/statement.rs:24` (synthesize the void expression with
  `self.ids.next()` and the return statement's span).

- **Interfaces/invariants:**
  - `never` is not a storable value type. The written-annotation path enforces
    this via `allow_never` → `TypeResolutionError::NeverNotAllowedHere`
    (`analysis/mod.rs:1323,1352`; `context.rs:506`). Accepting `never` in value
    positions must not open an inferred path around it — which today it already
    is (see step 5). Locals, parameters, fields, generic arguments and
    aggregate members must still reject `never`, with tests, not assumption.
  - A `never`-returning call keeps its runtime guard in every newly legal
    position, and the analyzer's site counter
    (`runtime_checks.rs`, `walk_expr`) already recurses into arguments and
    fields, so `PanicSupport` resolution needs no change — confirm, do not
    assume.
  - Operand order stays observable: operands before a diverging one execute,
    the diverging one's own effects execute, nothing after it does.
  - `guard_never_call` and `materialize_once` already guard on
    `is_current_terminated()`; `push_stmt`/`terminate`
    (`lower/function.rs:250,258`) assert on a terminated block.

- **Out of scope** (do not fix; step 12 records them):
  - `1 + exit(1)` — rejected by operator *applicability* checking, a different
    check needing its own decision about the compound expression's type.
  - `<i32>exit(1)` — a cast of a diverging operand. Note that `<void>` already
    propagates `Never` deliberately (`exprs/operators.rs:220-227`); the general
    cast case is a separate question.
  - `loop` is a statement, not an expression, so `consume(loop { })` is a parse
    error. Making `loop` an expression is a language change.
  - `x := consume(1)` binds a `void` local — the same missing gate as step 5,
    but for `void`. Fix `never` only here.
  - Index-expression type checking, already tracked separately in
    `known-issues.md`.

- **Risks/open questions — escalate, do not decide alone:**
  - Once `conversion_cost` accepts `never`, a `never` argument makes *every*
    overload viable on that parameter, so an overloaded call reports
    ambiguity. That is the intended honest outcome (the program must still name
    one function even though the call never happens) — but if the resulting
    diagnostic is unhelpful or an existing `core`/`std` call becomes ambiguous,
    stop and report rather than adding a tiebreak rule.
  - If typing an all-arms-diverge `if` as `Never` changes behaviour anywhere
    beyond `expr_diverges` — in particular if it makes a previously accepted
    `core`/`std` program fail — stop and report; that would mean a third
    consumer of the structural form exists.

## Implementation Plan

1. Add the value-position predicate beside `plan_coercion` in
   `compiler/omega-analyzer/src/analysis/mod.rs`, with a doc comment stating
   the `docs/language/` rule it implements and why it is not in
   `ResolvedType::accepts`. No behaviour change yet.
2. Adopt it at the ten value sites in the table above. After this step,
   `consume(exit(1))`, a `never` struct-literal field, a `never` array element,
   `x: i32 = exit(1)`, `x = exit(1)`, `return exit(1);` and `flag && exit(1)`
   all compile — this is issues 1 and 2.
3. Make `conversion_cost` (`analysis/mod.rs:1011`) agree, so overload viability
   cannot contradict step 2.
4. Type an all-arms-diverge `if` as `Never` in `analyze_if`
   (`exprs/mod.rs:388-398`), matching the codeblock path at `exprs/mod.rs:50`,
   then delete the `If` arm of `expr_diverges` (`stmts.rs:14-23`) and confirm
   `block_type`/`analyze_if`'s own `mismatch` scan still behave.
5. Gate inferred bindings in `analyze_walrus` (`stmts.rs:173-204`): reject a
   `never` initializer type with the existing
   `TypeResolutionError::NeverNotAllowedHere` diagnostic, so `x := exit(1)` no
   longer declares a `never`-typed local. Leave the `void` case alone.
6. Switch `lower_assignment_stmt` (`omega-mir/src/lower/function.rs:543`) to
   `lower_place_evaluated_once`, so the target's dynamic root/index components
   are evaluated into locals before the RHS lowers any control flow — this is
   issue 3.
7. Audit the remaining dynamic place/slice-bounds lowering the issue names as
   related candidates. Fix what is genuinely the same defect; record anything
   that is not rather than widening the change. In the same pass, verify that
   lowering an operand *after* a block is terminated cannot reach
   `push_stmt`/`terminate` on any path newly reachable after step 2 — the
   asserts at `lower/function.rs:250,258` are the failure mode.
8. Make `ReturnStmt::return_value` an `Option<ExpressionNode>`; accept
   `return;` in `parse_return` (`omega-parser/src/parser/statement.rs:214`) and
   carry the `Option` through the macro expander
   (`macros/expander.rs:504`).
9. Desugar in HIR lowering (`omega-hir/src/lower/statement.rs:24`): for `None`,
   synthesize a void expression with `self.ids.next()` and the return
   statement's span. Confirm the synthesized node reaches
   `return_value`/`return_slot` and the defer exit chain
   (`omega-mir/src/lower/function.rs:502`) correctly in a `void` function, and
   does not raise `unused_return_value`. This is issue 4.
10. Tests (see below).
11. Docs: update the `return-statement` production in
    `docs/language/grammar.md:428` to make the expression optional and delete
    the caveat at line 435. Check whether
    `docs/language/types-and-primitives.md#never` and
    `docs/language/control-flow-and-operators.md:15` need any wording change —
    expect none, since this task makes the implementation match what they
    already say; do not add caveats to them.
12. Remove the four resolved entries from `docs/issues/known-issues.md` (lines
    431, 449, 461, 509) and add one new entry covering the out-of-scope list:
    `never` as a binary or cast operand, `loop` not being an expression, and
    `void` inferred bindings.

## Testing

- **New/changed cases:**
  - Root conformance, positive: one `tests/<case>/` package exercising a
    `never` call directly in an argument, a struct-literal field, an array
    element, an annotated initializer, an assignment RHS, a `return`, and as an
    `&&`/`||` operand — proving each compiles and diverges where expected.
  - Root conformance, positive: an `if` whose arms all diverge used in an
    argument position, proving step 4.
  - Root conformance, positive: a `void` function using `return;` for an early
    exit, including one with a registered `defer`, proving the deferred
    statement still runs.
  - Crate-local Rust tests: the predicate itself in
    `omega-analyzer`; the assignment-target sequencing shape in
    `omega-mir`'s lowering tests; `omega-driver/tests/runtime_checks.rs` already
    asserts `NeverCallReturned` emission — extend it for the newly legal
    positions rather than duplicating it as a conformance case.
- **Specification trace:** `docs/language/types-and-primitives.md#never` ("A
  diverging expression is compatible with any expected expression type", "`never`
  is not a storable value type", "The check is on the call, not on every
  expression whose type is `never`"); `docs/language/grammar.md`'s
  `return-statement` production; `docs/language/control-flow-and-operators.md`
  for `defer` ordering on the `return;` path.
- **Negative/diagnostic cases** — each must assert the exact expected
  diagnostic, never merely that compilation failed. All of these are
  *compile failures*, so they belong in the root suite: when compilation
  fails, `bin/test-runner` compares the compiler's own stderr against the
  case's ordinary `expected.stderr`, which 43 existing cases already rely on
  (`t02b_reserved_type_names`, `t05e_generic_overload_ambiguity`, …). Do **not**
  use `expected.compiler.stderr` — that hook asserts compiler stderr after a
  *successful* compilation, which is not what these cases are.
  - `never` as a local type, a parameter type, a field type, a generic argument
    and an aggregate member still rejected with
    `TypeResolutionError::NeverNotAllowedHere`.
  - `x := exit(1)` rejected (step 5).
  - `return;` in a non-`void` function rejected with `ReturnTypeMismatch`.
  - `return <value>;` in a `void` function still rejected as before.
- **Regression coverage:** extend the `t44` family — `t44_never_call_returned`
  currently reaches argument position only through an `if` wrapper
  (`consume(before(), if flag { terminate_now() } else { 0 }, after())`) and
  already proves that earlier arguments run and later ones do not. Add the
  direct spellings now legal: `consume(before(), terminate_now(), after())`,
  and a `never` call as a struct-literal field, each with a C-defined callee
  that returns. `t44b`/`t44c` cover the function-pointer and dynamic-call
  contracts and should keep passing untouched.
  Add the sequencing regression for issue 3 from the reproduction in the issue
  entry: `*select_target(&mut value) = if true { panic$("rhs"); } else { 1 };`
  must print the target's marker before the panic diagnostic.
- **Commands/target coverage:** `./bin/test-runner <case>` for focused runs
  once `omgc` and the runtime objects are built, then `just test-all`. Any new
  case whose subject is a compiler-generated check must also be added to
  `RUNTIME_CHECK_CASES` in the `justfile`, which re-runs those cases at `-O3`;
  the sequencing and never-guard cases both qualify, so verify them at `-O0`
  and `-O3`.
