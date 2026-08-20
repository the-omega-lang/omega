# Evaluate compound-assign/increment-decrement target place exactly once

## Task Description

- **Deliverable:** `x op= y` and `x++`/`x--` evaluate a dynamic target place
  (e.g. an array index expression, or a computed pointer base) exactly once,
  matching `docs/language/bindings-and-mutability.md`: "`x op= y` has the
  same operator semantics as computing `x op y` and storing the result back
  into `x`, while evaluating the target place only once." Fixes the failing
  `tests/t20-operators` conformance case (`indexed-place-eval` expects
  `probe_calls == 1`, currently observes `2`).

- **Purpose:** `compiler/omega-analyzer/src/analysis/exprs/operators.rs`
  `analyze_compound_assign` (~724-760) and `analyze_incr_decr` (~417-479)
  both desugar into a plain `CheckedExpr::Assignment(CheckedAssignment {
  target: checked_place, value })` where `value` embeds a second
  `CheckedExpr::Place(checked_place.clone())` as the "read" side. Any
  side-effecting projection inside `checked_place` — concretely
  `CheckedProjection::Index { index_expr, .. }`, and a dynamic
  `CheckedPlaceRoot::Expr(_)` root — is therefore a live, independent
  sub-AST in two places in the checked tree. `compiler/omega-mir/src/lower/place.rs`
  (`lower_place`/`lower_projection`) lowers whichever `CheckedPlace` it is
  given from scratch every time it is called, so `target` (write) and the
  embedded read each independently re-lower `index_expr`, executing it
  twice. `counted[next_index()] += 100;` therefore calls `next_index()`
  twice instead of once.

- **Chosen direction (decided with the user, do not re-litigate):**
  Introduce a dedicated checked representation, `CheckedExpr::CompoundAssign
  (CheckedCompoundAssign)`, that owns exactly one `CheckedPlace` — no clone
  into a second read position. Add a dedicated MIR lowering path that lowers
  that place's dynamic components (`Index::index_expr`, and a dynamic
  `CheckedPlaceRoot::Expr`) into MIR locals exactly once via the existing
  "materialize into a local, read it back" idiom already used for if/match
  merges (`compiler/omega-mir/src/lower/function/control_flow.rs`:
  `declare_local(None, ty)` + `assign_local` + `local_expr`), then performs
  load → combine → store through the resulting address, reusing it for both
  the read and the write. Both `analyze_compound_assign` and
  `analyze_incr_decr` produce this same node shape, since they currently
  share the identical clone-based bug.

- **Rejected alternative:** an `omega-mir`-only fix that caches lowered
  index expressions in a `HashMap<HirId, LocalId>` keyed by the fact that
  `Clone` on a `CheckedPlace` happens to preserve `HirId`s on the embedded
  index subtree. Rejected because correctness would rest on an undocumented,
  unenforced cross-crate invariant (an analyzer-internal `Clone` behavior
  that `omega-mir` is not entitled to assume) rather than on the checked
  tree's own shape.

## Technical Details

- **Initial context boundary:** `compiler/omega-analyzer` (checked tree
  shape + `analyze_compound_assign`/`analyze_incr_decr`) and
  `compiler/omega-mir` (place lowering + expression lowering). Parser/HIR
  are untouched — `HirCompoundAssign`/`Expression::CompoundAssign` and the
  `++`/`--` HIR shape already carry `target`/`base` once; the duplication is
  introduced only during analysis. Backends (`omega-codegen`) consume MIR
  and need no changes: the fix produces ordinary `MirAssignment` +
  `MirBinaryOp`/`MirCast` nodes plus extra locals, all existing MIR
  vocabulary.

- **Affected files/symbols:**
  - `compiler/omega-analyzer/src/checked.rs`: add
    `CheckedExpr::CompoundAssign(CheckedCompoundAssign)` and
    ```rust
    pub struct CheckedCompoundAssign {
        pub place: CheckedPlace,
        pub read_cast: Option<(CastKind, ResolvedType)>,
        pub op: BinaryOp,
        pub value: Box<CheckedExprNode>,
        pub result_type: ResolvedType,
    }
    ```
    `read_cast` captures the coercion (if any) that would have been applied
    to the read side by `coerce_for_binary_op` (today this only fires for
    `Pointer` targets, whose `arithmetic_repr()` is `USize`); `result_type`
    is the type of the combined `op` result before it is stored back
    (mirrors what `combined.r#type` was in the old desugaring).
  - `compiler/omega-analyzer/src/analysis/exprs/operators.rs`:
    - `analyze_compound_assign` (~724-760): keep calling
      `self.analyze_binary_op(..)` with a throwaway placeholder
      `CheckedExprNode { kind: CheckedExpr::Place(checked_place.clone()), .. }`
      as the left operand — this preserves all existing validity/coercion/
      diagnostic behavior verbatim (spans, error kinds, always-true/false
      comparison check — the latter is a no-op here since compound-assign
      operators are never comparisons). The placeholder's clone of
      `checked_place` is discarded after this call; it must never be handed
      to MIR. Destructure the returned `combined.kind` (guaranteed
      `CheckedExpr::BinaryOp` by `analyze_binary_op`'s own contract, defined
      a few dozen lines above in the same file) to pull out: `read_cast`
      from `binary.left.kind` (`CheckedExpr::Cast(cast) => Some((cast.kind,
      cast.target_type))`, else `None`), `op: binary.op`, `value:
      binary.right`, `result_type: combined.r#type`. Build the final node as
      `CheckedExpr::CompoundAssign(CheckedCompoundAssign { place:
      checked_place, read_cast, op, value, result_type })` with outer
      `r#type: place_type` (unchanged from today).
    - `analyze_incr_decr` (~417-479): unlike compound-assign, this path
      never calls `analyze_binary_op` today (it hand-builds the `BinaryOp`
      node) and `place_type` is always a genuine `numeric_kind` type (never
      `Pointer`, so `arithmetic_repr()` is always `None`). Build
      `CheckedCompoundAssign { place: checked_place, read_cast: None, op,
      value: Box::new(one_node), result_type: place_type.clone() }`
      directly — no `analyze_binary_op` call needed, matching current
      behavior exactly.
  - `compiler/omega-mir/src/lower/place.rs`: refactor `lower_place` to
    delegate to a shared `lower_place_with(lowerer, place, lower_dynamic:
    impl FnMut(&mut FunctionLowerer, CheckedExprNode) -> MirExprNode)` that
    parametrizes exactly the two dynamic sites (`CheckedPlaceRoot::Expr`
    root, `CheckedProjection::Index.index_expr`) over how the embedded
    expression gets lowered. `lower_place(lowerer, place)` keeps today's
    behavior via `|lowerer, e| lowerer.lower_expr(e)`. Add
    `lower_place_evaluated_once(lowerer, place)` using a closure that lowers
    the expression and then materializes it once through a new
    `FunctionLowerer::materialize_once(&mut self, value: MirExprNode) ->
    MirExprNode` helper (declare a local via `declare_local(None,
    value.r#type.clone())`, `assign_local(value.id, value.span, local,
    value)`, return `self.local_expr(local, id, span)` — same idiom as
    `finish_merge` in `control_flow.rs`). The resulting `MirPlace` is then
    safe to lower once and reuse (clone) for both a load and a store,
    because its dynamic pieces are now plain local reads instead of
    re-evaluable expression trees. Non-dynamic places (no `Index`
    projection, no `Expr` root) pass through both functions identically
    with zero extra locals.
  - `compiler/omega-mir/src/lower/function/expr.rs`: add a match arm
    `CheckedExpr::CompoundAssign(compound) =>
    lowerer.lower_compound_assign_expr(id, span, r#type, compound)`
    alongside the existing `CheckedExpr::Assignment` arm (~59-68). New
    method (co-locate near `lower_place`/assignment lowering, e.g. in
    `place.rs` or a new small section of `expr.rs`):
    ```rust
    fn lower_compound_assign_expr(&mut self, id, span, r#type, compound: CheckedCompoundAssign) -> MirExprNode {
        let target = lower_place_evaluated_once(self, compound.place);
        let mut read = MirExprNode { id, span, r#type: target.r#type.clone(), kind: MirExpr::Place(target.clone()) };
        if let Some((kind, cast_type)) = compound.read_cast {
            read = MirExprNode { id, span, r#type: cast_type.clone(), kind: MirExpr::Cast(MirCast { kind, target_type: cast_type, base: Box::new(read) }) };
        }
        let value = Box::new(self.lower_expr(*compound.value));
        let combined = MirExprNode {
            id, span, r#type: compound.result_type,
            kind: MirExpr::BinaryOp(MirBinaryOp { op: compound.op, left: Box::new(read), right: value }),
        };
        MirExprNode { id, span, r#type, kind: MirExpr::Assignment(MirAssignment { target, value: Box::new(combined) }) }
    }
    ```
    No statement-position special case is needed: `CheckedExpr::CompoundAssign`
    is not control-flow (`is_control_flow_expr` stays `If | Match |
    Codeblock`), so it already flows through the existing
    `lower_plain_expr_stmt` → `lower_expr` → `push_stmt` path used for every
    other non-control-flow expression, in both statement and expression
    position.
  - Exhaustiveness follow-ups (compiler will report these as missing-match
    errors; fix each with the obvious structural-recursion arm, mirroring
    the existing `CheckedExpr::Assignment` arm in the same match):
    - `compiler/omega-analyzer/src/dead_code.rs` (`collect_expr`, ~91+):
      recurse into `compound.place` (via existing `collect_place`) and
      `compound.value`.
    - `compiler/omega-mir/src/lower/function/defer.rs` (`collect_expr`,
      ~54+): same recursive shape, for defer-capture analysis.
    - `compiler/omega-analyzer/src/comp_eval.rs` (`eval_expr` match, ~164+):
      compile-time evaluation. Add an arm that reads through `compound.place`,
      applies `read_cast` if present (reuse whatever const-eval already does
      for `CheckedExpr::Cast`, likely a small shared cast-application
      helper — check `eval_expr`'s `CheckedExpr::Cast` arm), combines with
      `compound.value` per `compound.op` (reuse `eval_binary_op` or its
      inner arithmetic helper), and writes the result back via
      `self.write_place(&compound.place, result.clone(), node.span)`,
      returning the result (mirrors the existing `CheckedExpr::Assignment`
      arm's shape: read/compute once, write once, return the written
      value). If compile-time evaluation cannot reasonably support
      compound-assign/incr-decr on a place with side-effecting components
      (unlikely — comp-eval bodies are already restricted), a `CompErrorKind`
      is preferable to a panic; check existing comp-eval error kinds before
      inventing one.
    - Any other non-exhaustive `match &_.kind { CheckedExpr::... }` the
      compiler flags (search was not exhaustive by design — trust the
      compiler here, not a manual grep sweep).

- **Interfaces/invariants:**
  - `CheckedExpr::Assignment`/`CheckedAssignment` and their existing
    lowering paths (`lower_assignment_stmt`, the `Assignment` arm in
    `expr.rs`, `defer.rs`, `comp_eval.rs`, `dead_code.rs`) are untouched —
    plain `=` keeps evaluating its target exactly as it does today (it
    already only lowers `assignment.target` once).
  - `analyze_binary_op`'s validity/coercion/diagnostic behavior must not
    change for ordinary binary expressions; it is reused unmodified by
    `analyze_compound_assign`; the plan only extracts data from its output.
  - The extraction in `analyze_compound_assign` relies on `analyze_binary_op`
    always returning `CheckedExpr::BinaryOp { left, right }` with `left`
    being exactly `coerce_for_binary_op(op, <the passed-in left operand>)`
    (either the operand unchanged or wrapped in one `CheckedExpr::Cast`
    layer). This is same-file, same-function coupling (not a cross-crate
    invariant) — add a short comment at the destructuring site stating this
    dependency so a future edit to `analyze_binary_op`'s operand handling is
    not made silently incompatible.
  - `materialize_once` must only be used where the place is genuinely read
    and written in the same lowering (compound-assign/incr-decr). Do not
    switch ordinary single-use place lowering (`lower_place`) to it — that
    would add unconditional extra locals/copies for every indexed access
    with no benefit, contradicting "abstractions compile away" / no
    surprise cost.

- **Out of scope:** `Storage::Comp` places (compile-time-only bindings) —
  `analyze_place_operand`/`require_mutable_place` already govern whether a
  compound-assign/incr-decr target is legal there; no new handling is
  planned beyond the `comp_eval.rs` arm above. No change to
  `omega-codegen`, `omega-hir`, or `omega-parser`. No change to pointer
  compound-assignment semantics beyond preserving today's behavior exactly
  (the existing `read_cast`/`Pointer`→`USize` coercion path is preserved,
  not redesigned).

- **Risks/open questions:** none identified that require stopping — the
  design was already reviewed with the user. If, while implementing the
  `comp_eval.rs` arm, no existing helper cleanly reapplies a `CastKind` to a
  `ConstValue` (i.e. compile-time cast application isn't already factored
  out of the `CheckedExpr::Cast` arm), a developer should factor a small
  shared helper rather than duplicating cast-evaluation logic inline —
  this is a local implementation decision, not one requiring escalation.

## Implementation Plan

1. `compiler/omega-analyzer/src/checked.rs`: add `CheckedCompoundAssign` and
   the `CheckedExpr::CompoundAssign` variant.
2. `compiler/omega-analyzer/src/analysis/exprs/operators.rs`: rewrite
   `analyze_compound_assign` and `analyze_incr_decr` to build
   `CheckedExpr::CompoundAssign` as specified above, with `checked_place`
   owned exactly once (the placeholder clone used for
   `analyze_compound_assign`'s type-check call is intentionally discarded,
   never stored in the returned node).
3. `cargo build -p omega-analyzer` (or full workspace) and fix any other
   exhaustive `CheckedExpr` matches inside `omega-analyzer` the compiler
   flags (`dead_code.rs`, `comp_eval.rs`, and any test helpers under
   `comp_eval/tests.rs` that construct/match `CheckedExpr` exhaustively).
4. `compiler/omega-mir/src/lower/place.rs`: refactor `lower_place`/
   `lower_projection` into the shared `lower_place_with`/
   `lower_projection_with` parametrized over dynamic-expression lowering;
   add `lower_place_evaluated_once` and `FunctionLowerer::materialize_once`.
5. `compiler/omega-mir/src/lower/function/expr.rs` (or `place.rs`): add
   `lower_compound_assign_expr` and the new `CheckedExpr::CompoundAssign`
   match arm.
6. `cargo build -p omega-mir` (or full workspace) and fix remaining
   exhaustive-match compile errors (`defer.rs`, any others flagged).
7. `cargo build` (full workspace) to confirm `omega-codegen`/`omgc` need no
   changes (they consume MIR, which only grew ordinary node shapes).
8. Add the `++`/`--` place-evaluation-count assertion to
   `tests/t20-operators/t20-operators.omg` (step 1 of Testing below) and
   regenerate `expected.stdout` for the now-single-evaluation
   `indexed-place-eval` line plus the new increment/decrement line.
9. Build runtime + run the focused conformance case, then the full suite
   (Testing below).

## Testing

- **New/changed cases:**
  - `tests/t20-operators/t20-operators.omg`: the existing
    `counted[next_index()] += 100;` / `indexed-place-eval` assertion
    (~101-104) already proves compound-assign evaluates the index once;
    update `expected.stdout` line 13 from `indexed-place-eval: 2 102` back
    to `indexed-place-eval: 1 102` once the fix lands.
  - Add an analogous `++`/`--` case in the same file/section (no existing
    test in the whole `tests/` suite exercises `++`/`--` at all): reset
    `probe_calls`, then something like `counted[next_index()] += 1;`
    replaced/supplemented with `counted[next_index()]++;`, print
    `probe_calls` and the resulting element, and add the expected line to
    `expected.stdout`. Keep it inline with the file's existing terse
    single-line-per-concept style (see the `# --- operate-and-assign`
    section) rather than adding a new large block.
- **Specification trace:** `docs/language/bindings-and-mutability.md`,
  "Compound assignment and increment/decrement" section — "evaluating the
  target place only once" for `op=`, and `++`/`--` "require the same
  mutability checks as assignment" (place-evaluation-once is the same
  underlying place-mutation contract).
- **Negative/diagnostic cases:** none required — this is a pure
  runtime-observable-behavior fix; no new diagnostic paths are introduced
  (`AnalysisErrorKind::CompoundAssignTargetNotAPlace`/
  `IncrementTargetNotAPlace`/mutability errors are unchanged, since
  `analyze_place_operand`/`require_mutable_place` calls are untouched).
- **Regression coverage:** run the full `tests/t20-operators` case (covers
  arithmetic, logical, bitwise, compound-assign, precedence, prefix-vs-infix
  `*`/`&` — broad operator surface that would catch a lowering regression),
  plus the full suite since `CheckedPlace`/MIR place lowering is shared
  infrastructure touched by many features (structs/unions, enums, generics,
  slicing all build places).
- **Commands/target coverage:** `cargo build` (workspace) for compile-time
  exhaustiveness/type correctness first; then `just test-all` for the full
  gate (builds `omgc` + runtime + runs `bin/test-runner` over all cases);
  for fast iteration once artifacts are built, `./bin/test-runner
  t20-operators`.
