//! Lowers one function/method body -- a `CheckedBlock` tree -- into a
//! [`crate::body::MirBody`] control-flow graph. This is the direct
//! data-producing analogue of `omega_codegen::Codegen`'s
//! `emit_block`/`emit_expr_stmt`/`emit_if`/`emit_match`/`emit_while`/
//! `emit_for`/`process_statement`/`process_decl` -- see
//! `docs/16-mir-and-codegen.md` for the full rationale.
//!
//! `FunctionLowerer` plays the role `Codegen`'s own per-function fields
//! (`stack_slots`/`local_args`/`loop_stack`/`defer_flags`/`defer_bodies`/
//! `return_block`) play today, except it builds a graph of [`MirBlockData`]
//! (kept in `blocks`, indexed by [`BlockId`]) instead of directly emitting
//! Cranelift IR against a `FunctionBuilder`.
//!
//! **The "did this diverge" question, without a `BlockOutcome`:** a block's
//! `terminator` starts `None` (see [`BuilderBlock`]) and is set at most
//! once; `is_current_terminated` asks exactly the question `BlockOutcome`
//! used to answer, and is used the same way -- once true, nothing may be
//! appended to that block, and whatever `Vec<CheckedStmt>`/tail is being
//! walked stops early (dead code, same as today). Marking a
//! zero-predecessor `if`/`match` merge block `Unreachable` immediately
//! (rather than leaving its terminator unset) is what makes this check
//! alone sufficient -- no separate return value/enum needed. `lower_if`/
//! `lower_match`'s chain-building helpers still return a `bool` ("did any
//! arm actually reach the merge block"), which is that same signal
//! threaded through the recursion.

use crate::body::{
    MirAddressOf, MirArrayLiteral, MirAssignment, MirBinaryOp, MirBlockData, MirBody, MirCast, MirDynamicCall,
    MirEnumConstruct, MirExpr, MirExprNode, MirFieldInit, MirFunctionCall, MirLocalDecl, MirPlace, MirPlaceRoot,
    MirSlice, MirSpecCoerce, MirStructLiteral, MirTerminator, MirUnionConstruct,
};
use crate::ids::{BlockId, LocalId};
use super::place::place_align;
use omega_analyzer::checked::{
    CheckedBlock, CheckedBreak, CheckedContinue, CheckedDefer, CheckedExpr, CheckedExprNode, CheckedFor, CheckedIf,
    CheckedLoop, CheckedMatch, CheckedMatchArm, CheckedParam, CheckedPlace, CheckedPlaceRoot, CheckedProjection,
    CheckedStmt, CheckedStructLiteralField, CheckedWhile,
};
use omega_analyzer::resolved_type::ResolvedType;
use omega_hir::HirId;
use omega_parser::prelude::Span;
use std::collections::HashMap;

/// A block still under construction -- `terminator: None` means exactly
/// what `BlockOutcome`'s absence used to mean: control hasn't left this
/// block yet. Converted to a real [`MirBlockData`] by `finish`, which is
/// also where an internal lowering bug (a block nobody ever terminated)
/// would panic.
struct BuilderBlock {
    statements: Vec<MirExprNode>,
    terminator: Option<MirTerminator>,
}

#[derive(Clone, Copy)]
struct LoopTargets {
    break_block: BlockId,
    continue_block: BlockId,
}

pub(crate) struct FunctionLowerer {
    locals: Vec<MirLocalDecl>,
    /// The lowering-time counterpart of `Codegen::stack_slots`/`local_args`
    /// -- maps a declared local/parameter's own `HirId` to the `LocalId`
    /// its `MirLocalDecl` lives at. `pub(super)` so `crate::lower::place`
    /// can resolve a `Storage::Local`/`Storage::Parameter` place root.
    pub(super) local_of: HashMap<HirId, LocalId>,
    blocks: Vec<BuilderBlock>,
    current: BlockId,
    loop_stack: Vec<(HirId, LoopTargets)>,
    /// `Some` once the return type is known non-`Void` -- every `return`/
    /// fallthrough writes its value here instead of threading it through
    /// block params, since (unlike an `if`/`match` join) the exit chain may
    /// be several blocks deep (see `lower`'s defer-chain construction) and
    /// block params only carry a value across one edge.
    return_slot: Option<LocalId>,
    /// The function's own return type -- kept around so the `return`/
    /// tail-position fast path (see `lower_control_flow_stmt`'s doc
    /// comment) can pass it to `lower_control_flow_into` without needing a
    /// value in hand yet (it's needed *before* any arm has been lowered).
    return_type: ResolvedType,
    /// The first block of the function's shared exit chain -- every
    /// `return`, and the body's own implicit fallthrough, `Goto` here
    /// (see `lower`).
    exit_chain_start: BlockId,
    /// A `defer`'s body, stashed here (moved out of the tree) by
    /// `lower_stmt`'s `Defer` arm, for `lower`'s exit-chain construction to
    /// consume once the whole function body has been walked -- the
    /// lowering-time counterpart of `Codegen::defer_bodies`.
    defer_bodies: HashMap<HirId, CheckedBlock>,
    /// A `defer`'s own `HirId` -> its (synthetic, `source: None`) flag
    /// local -- deliberately *not* `local_of` (which only maps a real
    /// user declaration/parameter to its local): a `defer`'s flag has no
    /// declaring `HirId` of its own to be looked up *by* the way a
    /// variable is, only an owning `defer` statement, so it gets its own
    /// map, populated by `lower`'s pre-pass and read back by `lower_stmt`'s
    /// `Defer` arm.
    defer_flag_of: HashMap<HirId, LocalId>,
}

impl FunctionLowerer {
    /// Lowers one function/method's parameter list and body into a
    /// [`MirBody`]. `fn_id`/`fn_span` are used only as a fallback id/span
    /// for the handful of nodes this synthesizes with no source counterpart
    /// of their own (the final exit chain's return-value read) -- purely
    /// diagnostic bookkeeping, never read by codegen.
    pub(crate) fn lower(
        params: &[CheckedParam],
        body: CheckedBlock,
        return_type: &ResolvedType,
        fn_id: HirId,
        fn_span: Span,
    ) -> MirBody {
        let mut lowerer = FunctionLowerer {
            locals: Vec::new(),
            local_of: HashMap::new(),
            blocks: Vec::new(),
            current: BlockId(0),
            loop_stack: Vec::new(),
            return_slot: None,
            return_type: return_type.clone(),
            exit_chain_start: BlockId(0),
            defer_bodies: HashMap::new(),
            defer_flag_of: HashMap::new(),
        };

        for param in params {
            lowerer.declare_local(Some(param.id), param.r#type.clone());
        }
        let arg_count = lowerer.locals.len();

        // Minted first, deliberately, so the entry block is always `BlockId(0)`
        // (see `MirBody::blocks`'s doc comment) -- the exit chain reserved
        // just below needs its own block ids known before the body is
        // walked, but nothing requires *this* block to be minted after them.
        let entry = lowerer.new_block();

        // Defer pre-pass: every `defer`'s id/span must be known before the
        // real walk begins, since a `return` nested arbitrarily deep needs
        // the exit chain's first block id up front -- see
        // `collect_defer_ids`'s own doc comment.
        let mut defers = Vec::new();
        collect_defer_ids(&body, &mut defers);
        let defer_flags: Vec<LocalId> =
            defers.iter().map(|_| lowerer.declare_local(None, ResolvedType::Bool)).collect();
        for (&flag, (id, _)) in defer_flags.iter().zip(&defers) {
            lowerer.defer_flag_of.insert(*id, flag);
        }

        lowerer.return_slot =
            (!matches!(return_type, ResolvedType::Void)).then(|| lowerer.declare_local(None, return_type.clone()));

        // Reserve the exit chain's blocks up front -- one check/run pair
        // per defer (processed FILO, last-declared first, below), plus the
        // final block that performs the real `Return`. Left un-terminated
        // until every defer body has actually been extracted from the tree
        // by the walk that follows.
        let check_blocks: Vec<BlockId> = defers.iter().map(|_| lowerer.new_block()).collect();
        let run_blocks: Vec<BlockId> = defers.iter().map(|_| lowerer.new_block()).collect();
        let final_block = lowerer.new_block();
        // `check_blocks`/`run_blocks` are indexed in *declaration* order
        // (matching `defers`), but the chain itself must run FILO -- the
        // *last*-declared defer torn down first -- so the chain starts at
        // the highest index, not `[0]`.
        lowerer.exit_chain_start = check_blocks.last().copied().unwrap_or(final_block);

        lowerer.current = entry;

        // One flag per defer, initialized `false` here -- unconditionally,
        // before the body is walked for real -- so a path that never
        // reaches a given `defer` reads back `false` in the exit chain,
        // exactly like `Codegen::defer_flags`' entry-block zero-init today.
        for (&flag, (id, span)) in defer_flags.iter().zip(&defers) {
            lowerer.assign_local(*id, *span, flag, ResolvedType::Bool, bool_literal(*id, *span, false));
        }

        // Not `lower_block_as_stmt` -- analysis guarantees this body either
        // ends in a statement-level `return` or a tail expression whose
        // value *is* the function's own return value (see
        // `CheckedFunctionDef::body`'s own doc comment), so a fall-through
        // here needs its tail routed into `return_slot`, exactly like an
        // explicit trailing `return` would -- `lower_function_body` is
        // `lower_block_into`'s analogue for that (`return_slot`, not a
        // merge block, is the destination).
        let fell_through = lowerer.lower_function_body(body);
        if fell_through {
            lowerer.set_terminator(MirTerminator::Goto(lowerer.exit_chain_start));
        }

        // Every defer's body has now been stashed by `lower_stmt`'s `Defer`
        // arm -- fill in the reserved chain blocks, FILO (the *last*-
        // declared defer tears down first).
        for (i, (id, span)) in defers.iter().enumerate().rev() {
            let check = check_blocks[i];
            let run = run_blocks[i];
            // The chain runs high-index-to-low (see `exit_chain_start`'s
            // own comment) -- `next` is the *previous* declaration's check
            // block (one step closer to `[0]`), or `final_block` once `[0]`
            // itself (the *first*-declared defer, torn down last) is done.
            let next = if i == 0 { final_block } else { check_blocks[i - 1] };
            let flag = defer_flags[i];

            lowerer.current = check;
            let flag_read = MirExprNode {
                id: *id,
                span: *span,
                r#type: ResolvedType::Bool,
                kind: MirExpr::Place(MirPlace {
                    root: MirPlaceRoot::Local { id: flag, r#type: ResolvedType::Bool },
                    projections: vec![],
                    r#type: ResolvedType::Bool,
                    align: place_align(&ResolvedType::Bool),
                }),
            };
            lowerer.set_terminator(MirTerminator::Branch { condition: flag_read, then_block: run, else_block: next });

            lowerer.current = run;
            let defer_body = lowerer
                .defer_bodies
                .remove(id)
                .expect("every collected defer is visited unconditionally during the walk above");
            let fell_through = lowerer.lower_block_as_stmt(defer_body);
            assert!(fell_through, "a defer body can never diverge -- analysis rejects return/break/continue inside one");
            lowerer.set_terminator(MirTerminator::Goto(next));
        }

        lowerer.current = final_block;
        let return_value = lowerer.return_slot.map(|slot| MirExprNode {
            id: fn_id,
            span: fn_span,
            r#type: return_type.clone(),
            kind: MirExpr::Place(MirPlace {
                root: MirPlaceRoot::Local { id: slot, r#type: return_type.clone() },
                projections: vec![],
                r#type: return_type.clone(),
                align: place_align(&return_type),
            }),
        });
        lowerer.set_terminator(MirTerminator::Return(return_value));

        lowerer.finish(arg_count)
    }

    fn declare_local(&mut self, source: Option<HirId>, r#type: ResolvedType) -> LocalId {
        let id = LocalId(self.locals.len() as u32);
        if let Some(hir_id) = source {
            self.local_of.insert(hir_id, id);
        }
        self.locals.push(MirLocalDecl { source, r#type });
        id
    }

    fn new_block(&mut self) -> BlockId {
        let id = BlockId(self.blocks.len() as u32);
        self.blocks.push(BuilderBlock { statements: Vec::new(), terminator: None });
        id
    }

    fn is_current_terminated(&self) -> bool {
        self.blocks[self.current.0 as usize].terminator.is_some()
    }

    fn push_stmt(&mut self, expr: MirExprNode) {
        debug_assert!(!self.is_current_terminated(), "cannot append a statement to an already-terminated block");
        self.blocks[self.current.0 as usize].statements.push(expr);
    }

    fn set_terminator(&mut self, terminator: MirTerminator) {
        debug_assert!(!self.is_current_terminated(), "a block can only be terminated once");
        self.blocks[self.current.0 as usize].terminator = Some(terminator);
    }

    fn assign_local(&mut self, id: HirId, span: Span, local: LocalId, r#type: ResolvedType, value: MirExprNode) {
        let target = MirPlace {
            root: MirPlaceRoot::Local { id: local, r#type: r#type.clone() },
            projections: vec![],
            r#type: r#type.clone(),
            align: place_align(&r#type),
        };
        self.push_stmt(MirExprNode {
            id,
            span,
            r#type: ResolvedType::Void,
            kind: MirExpr::Assignment(MirAssignment { target, value: Box::new(value) }),
        });
    }

    fn loop_target(&self, loop_id: HirId) -> LoopTargets {
        self.loop_stack
            .iter()
            .rev()
            .find(|(id, _)| *id == loop_id)
            .map(|(_, targets)| *targets)
            .expect("checked module guarantees a break/continue's loop_id is a currently-enclosing loop")
    }

    fn finish(self, arg_count: usize) -> MirBody {
        let blocks = self
            .blocks
            .into_iter()
            .enumerate()
            .map(|(i, b)| MirBlockData {
                statements: b.statements,
                terminator: b
                    .terminator
                    .unwrap_or_else(|| panic!("omega-mir lowering bug: block {i} was never terminated")),
            })
            .collect();
        MirBody { locals: self.locals, arg_count, blocks }
    }

    /// Lowers a whole statement sequence, stopping early (leaving any
    /// remaining statements un-lowered, i.e. dead code) the moment
    /// `self.current` terminates -- the direct analogue of `emit_block`'s
    /// per-statement loop.
    fn lower_stmts(&mut self, stmts: Vec<CheckedStmt>) {
        for stmt in stmts {
            if self.is_current_terminated() {
                break;
            }
            self.lower_stmt(stmt);
        }
    }

    fn lower_stmt(&mut self, stmt: CheckedStmt) {
        match stmt {
            CheckedStmt::Declaration(decl) => {
                self.declare_local(Some(decl.id), decl.r#type);
            }
            CheckedStmt::ExternDeclaration(_) => {
                todo!("extern declarations inside a function body are not yet implemented");
            }
            CheckedStmt::Expression(expr) => self.lower_expr_stmt(expr),
            CheckedStmt::Return(expr) => {
                // Fast path: `return if/match/{ }` -- see
                // `lower_control_flow_stmt`'s doc comment for why routing
                // straight into the exit chain (instead of through
                // `lower_expr`'s always-safe-but-allocates-a-temp general
                // path) needs no merge block of its own here: every reached
                // arm already terminates by jumping to `exit_chain_start`
                // directly, so there is no "afterward" to position
                // `self.current` at.
                if is_control_flow_expr(&expr.kind) {
                    let result = self.return_slot;
                    let result_type = self.return_type.clone();
                    self.lower_control_flow_into(expr, self.exit_chain_start, result, result_type);
                    return;
                }
                let diverges = expr.r#type == ResolvedType::Never;
                let value = self.lower_expr(expr);
                if self.is_current_terminated() {
                    return;
                }
                if let Some(slot) = self.return_slot {
                    let id = value.id;
                    let span = value.span;
                    let r#type = value.r#type.clone();
                    self.assign_local(id, span, slot, r#type, value);
                }
                // `return exit(1);` (a `never`-returning value, unlike a
                // bare `return;`) has no real value for the exit chain's
                // own `return_slot` read to find -- `assign_local` above
                // still runs so the call itself is emitted (it's the RHS
                // of that assignment), but routing through the *normal*
                // exit chain afterward would read whatever garbage that
                // meaningless assignment left behind. Trap instead, same
                // reasoning as `lower_expr_stmt`'s identical case.
                self.set_terminator(if diverges { MirTerminator::Unreachable } else { MirTerminator::Goto(self.exit_chain_start) });
            }
            CheckedStmt::While(CheckedWhile { id, condition, body, .. }) => {
                self.lower_while(id, condition, body);
            }
            CheckedStmt::Loop(CheckedLoop { id, body, .. }) => {
                self.lower_loop(id, body);
            }
            CheckedStmt::For(for_loop) => {
                let CheckedFor { id, init, condition, post, body, .. } = *for_loop;
                self.lower_for(id, init, condition, post, body);
            }
            CheckedStmt::Break(CheckedBreak { loop_id, .. }) => {
                let target = self.loop_target(loop_id).break_block;
                self.set_terminator(MirTerminator::Goto(target));
            }
            CheckedStmt::Continue(CheckedContinue { loop_id, .. }) => {
                let target = self.loop_target(loop_id).continue_block;
                self.set_terminator(MirTerminator::Goto(target));
            }
            CheckedStmt::Defer(CheckedDefer { id, span, body }) => {
                let flag = *self
                    .defer_flag_of
                    .get(&id)
                    .expect("every defer's flag local is pre-allocated by the pre-pass in `lower`");
                self.assign_local(id, span, flag, ResolvedType::Bool, bool_literal(id, span, true));
                self.defer_bodies.insert(id, body);
            }
        }
    }

    /// A bare expression statement -- `expr;`. Two fast paths, both there
    /// purely to avoid the general (always-safe, but always-allocates-a-
    /// temp-local) path's cost for the two shapes that make up the
    /// overwhelming majority of real `if`/`match`-as-a-value use: a bare
    /// `if a { .. } else { .. };` statement, and (since a walrus/typed
    /// declaration desugars to a `Declaration` followed by exactly this)
    /// `x := if a { .. } else { .. };`. See `lower_control_flow_stmt`'s doc
    /// comment for why these are safe to skip the temp for -- neither has
    /// any sibling sub-expression lowered after it within the same
    /// statement, which is the one thing that makes a shared temp
    /// necessary in general.
    fn lower_expr_stmt(&mut self, expr: CheckedExprNode) {
        let CheckedExprNode { id, span, r#type, kind } = expr;

        if let CheckedExpr::Assignment(assignment) = kind {
            if is_control_flow_expr(&assignment.value.kind) {
                let target = self.lower_place(assignment.target);
                if target.projections.is_empty()
                    && let MirPlaceRoot::Local { id: local_id, r#type: local_type } = target.root
                {
                    self.lower_control_flow_stmt(*assignment.value, Some((local_id, local_type)));
                    return;
                }
                // Not a bare local (a field/deref/index target) -- fall
                // back to the general path, reusing the place already
                // lowered above rather than lowering it twice (which would
                // double-evaluate a side-effecting index expression).
                let diverges = assignment.value.r#type == ResolvedType::Never;
                let value = Box::new(self.lower_expr(*assignment.value));
                let node = MirExprNode {
                    id,
                    span,
                    r#type,
                    kind: MirExpr::Assignment(MirAssignment { target, value }),
                };
                if self.is_current_terminated() {
                    return;
                }
                self.push_stmt(node);
                // See the general path's identical `diverges` handling
                // below -- an assignment whose own value never actually
                // gets produced needs the same trap, not a normal
                // fallthrough.
                if diverges {
                    self.set_terminator(MirTerminator::Unreachable);
                }
                return;
            }
            let target = self.lower_place(assignment.target);
            let diverges = assignment.value.r#type == ResolvedType::Never;
            let value = Box::new(self.lower_expr(*assignment.value));
            let node =
                MirExprNode { id, span, r#type, kind: MirExpr::Assignment(MirAssignment { target, value }) };
            if self.is_current_terminated() {
                return;
            }
            self.push_stmt(node);
            if diverges {
                self.set_terminator(MirTerminator::Unreachable);
            }
            return;
        }

        if is_control_flow_expr(&kind) {
            self.lower_control_flow_stmt(CheckedExprNode { id, span, r#type, kind }, None);
            return;
        }

        let diverges = r#type == ResolvedType::Never;
        let node = self.lower_expr(CheckedExprNode { id, span, r#type, kind });
        if self.is_current_terminated() {
            return;
        }
        self.push_stmt(node);
        // A bare call to a `never`-returning function needs an explicit
        // trap: nothing else follows it in the checked tree, so without
        // this the block would be left un-terminated. Also the backstop if
        // an `extern` declared `never` actually returns anyway -- see
        // primitives.md's "never" section.
        if diverges {
            self.set_terminator(MirTerminator::Unreachable);
        }
    }

    /// `lower_block_as_stmt`'s counterpart for the function's own top-level
    /// body: its tail expression (if any) is the function's own return
    /// value, so it's written to `return_slot` -- exactly like an explicit
    /// trailing `return` would -- instead of being pushed as a discarded
    /// statement. Returns whether control fell off the end normally
    /// (`true`, meaning the caller still needs to route into the exit
    /// chain) or already diverged (`false`).
    fn lower_function_body(&mut self, block: CheckedBlock) -> bool {
        self.lower_stmts(block.stmts);
        if self.is_current_terminated() {
            return false;
        }
        if let Some(tail) = block.tail {
            // Fast path -- see `lower_stmt`'s `Return` arm, which this
            // mirrors exactly (an implicit tail-return is otherwise
            // identical to an explicit one): every reached arm already
            // jumps straight to the exit chain, so there is no fallthrough
            // left for the caller to route there itself.
            if is_control_flow_expr(&tail.kind) {
                let result = self.return_slot;
                let result_type = self.return_type.clone();
                self.lower_control_flow_into(*tail, self.exit_chain_start, result, result_type);
                return false;
            }
            let diverges = tail.r#type == ResolvedType::Never;
            let value = self.lower_expr(*tail);
            if self.is_current_terminated() {
                return false;
            }
            if let Some(slot) = self.return_slot {
                let id = value.id;
                let span = value.span;
                let r#type = value.r#type.clone();
                self.assign_local(id, span, slot, r#type, value);
            }
            // The function's own tail expression never actually produced
            // a value -- same reasoning as `lower_stmt`'s `Return` arm:
            // trap here instead of letting the caller route this into the
            // exit chain to read `return_slot` back.
            if diverges {
                self.set_terminator(MirTerminator::Unreachable);
                return false;
            }
        }
        true
    }

    /// Lowers `block` in a position where its own value (if any) is
    /// discarded -- a `while`/`for` body, or a `defer`'s own body. Returns
    /// whether control fell off the end normally (`true`) or already
    /// diverged (`false`), the same "reached" signal `lower_block_into`
    /// returns for a value-producing position.
    fn lower_block_as_stmt(&mut self, block: CheckedBlock) -> bool {
        self.lower_stmts(block.stmts);
        if self.is_current_terminated() {
            return false;
        }
        if let Some(tail) = block.tail {
            let diverges = tail.r#type == ResolvedType::Never;
            let node = self.lower_expr(*tail);
            if self.is_current_terminated() {
                return false;
            }
            self.push_stmt(node);
            if diverges {
                self.set_terminator(MirTerminator::Unreachable);
                return false;
            }
        }
        true
    }

    /// Dispatches an `If`/`Match`/`Codeblock`-shaped `expr` straight into a
    /// caller-supplied `merge`/`result` instead of allocating its own (the
    /// mechanism behind every fast path in this file -- `lower_expr_stmt`'s
    /// two, `lower_stmt`'s `Return` arm, and `lower_function_body`'s tail).
    /// Panics if `expr` isn't one of those three -- every caller checks
    /// `is_control_flow_expr` first.
    fn lower_control_flow_into(
        &mut self,
        expr: CheckedExprNode,
        merge: BlockId,
        result: Option<LocalId>,
        result_type: ResolvedType,
    ) -> bool {
        match expr.kind {
            CheckedExpr::If(CheckedIf { branches, else_branch }) => {
                self.lower_if_chain(branches.into_iter(), else_branch, merge, result, result_type)
            }
            CheckedExpr::Match(CheckedMatch { arms, else_branch }) => {
                self.lower_match_chain(arms.into_iter(), else_branch, merge, result, result_type)
            }
            CheckedExpr::Codeblock(block) => self.lower_block_into(block, merge, result, result_type),
            _ => unreachable!("lower_control_flow_into is only ever called after is_control_flow_expr matched"),
        }
    }

    /// `lower_control_flow_into`'s counterpart for a *statement* position
    /// (a bare `if`/`match` statement, or `place = if/match/{ }`): unlike
    /// `return`/tail position, where `merge` is the already-existing exit
    /// chain, a statement has to keep going afterward, so this mints `merge`
    /// fresh and positions `self.current` on it once every arm is lowered --
    /// the same "mark unreachable if nothing reached it" finalization
    /// `finish_merge` does for the value-producing path, just with no value
    /// to hand back.
    fn lower_control_flow_stmt(&mut self, expr: CheckedExprNode, result: Option<(LocalId, ResolvedType)>) {
        let merge = self.new_block();
        let (result_id, result_type) = match result {
            Some((id, r#type)) => (Some(id), r#type),
            None => (None, ResolvedType::Void),
        };
        let reached = self.lower_control_flow_into(expr, merge, result_id, result_type);
        self.current = merge;
        if !reached {
            self.set_terminator(MirTerminator::Unreachable);
        }
    }

    /// Lowers `block` as one arm of an `if`/`match`/bare-`{}` value join:
    /// its tail expression (if any -- absent means this arm is `Void`, and
    /// analysis guarantees every arm agrees on that when one is) is written
    /// into `result` before jumping to `merge` -- see `MirBlockData`'s doc
    /// comment for why a synthetic local, not a block argument, is what
    /// carries the value across this jump. `result: None` means nobody
    /// needs this arm's value at all (the bare-statement/`return`/tail
    /// fast paths -- see `lower_control_flow_stmt`/`lower_stmt`'s `Return`
    /// arm/`lower_function_body`), in which case the tail is still lowered
    /// and still pushed as an ordinary (side-effect-only) statement -- it
    /// just isn't copied anywhere. Returns whether `merge` was actually
    /// reached.
    fn lower_block_into(
        &mut self,
        block: CheckedBlock,
        merge: BlockId,
        result: Option<LocalId>,
        result_type: ResolvedType,
    ) -> bool {
        self.lower_stmts(block.stmts);
        if self.is_current_terminated() {
            return false;
        }
        if let Some(tail) = block.tail {
            let diverges = tail.r#type == ResolvedType::Never;
            let value = self.lower_expr(*tail);
            if self.is_current_terminated() {
                return false;
            }
            match result {
                Some(result) => {
                    let id = value.id;
                    let span = value.span;
                    self.assign_local(id, span, result, result_type, value);
                }
                None => self.push_stmt(value),
            }
            // This arm's own tail never actually produced the value the
            // merge block expects -- e.g. `if cond { exit(1) } else { 42
            // }`, where this is the `exit(1)` arm. Trap here instead of
            // joining `merge` as if a real value were coming; the
            // *overall* `if`/`match` still resolves fine as long as some
            // other arm does reach it (its own type is what the whole
            // expression ends up typed as).
            if diverges {
                self.set_terminator(MirTerminator::Unreachable);
                return false;
            }
        }
        self.set_terminator(MirTerminator::Goto(merge));
        true
    }

    fn lower_while(&mut self, loop_id: HirId, condition: CheckedExprNode, body: CheckedBlock) {
        let header = self.new_block();
        let body_blk = self.new_block();
        let exit = self.new_block();

        self.set_terminator(MirTerminator::Goto(header));

        self.current = header;
        let cond = self.lower_expr(condition);
        if self.is_current_terminated() {
            return;
        }
        self.set_terminator(MirTerminator::Branch { condition: cond, then_block: body_blk, else_block: exit });

        self.current = body_blk;
        self.loop_stack.push((loop_id, LoopTargets { break_block: exit, continue_block: header }));
        let fell_through = self.lower_block_as_stmt(body);
        self.loop_stack.pop();
        if fell_through {
            self.set_terminator(MirTerminator::Goto(header));
        }

        self.current = exit;
    }

    /// `loop { body }` -- the same shape as `lower_while`, minus the
    /// header/condition-check block: there's no condition to branch on, so
    /// entry goes straight to `body_blk`, which falls through back to
    /// itself rather than to a separate header. `exit` is still always
    /// created, even when nothing in `body` ever `break`s (i.e. it's
    /// statically unreachable) -- exactly like `while true { }`'s `exit`
    /// already is today, and for the same reason: it still needs *some*
    /// terminator (whatever code follows the loop provides one), and MIR
    /// has no trouble with a block that's simply dead at runtime.
    fn lower_loop(&mut self, loop_id: HirId, body: CheckedBlock) {
        let body_blk = self.new_block();
        let exit = self.new_block();

        self.set_terminator(MirTerminator::Goto(body_blk));

        self.current = body_blk;
        self.loop_stack.push((loop_id, LoopTargets { break_block: exit, continue_block: body_blk }));
        let fell_through = self.lower_block_as_stmt(body);
        self.loop_stack.pop();
        if fell_through {
            self.set_terminator(MirTerminator::Goto(body_blk));
        }

        self.current = exit;
    }

    fn lower_for(
        &mut self,
        loop_id: HirId,
        init: Vec<CheckedStmt>,
        condition: CheckedExprNode,
        post: Option<CheckedExprNode>,
        body: CheckedBlock,
    ) {
        self.lower_stmts(init);
        if self.is_current_terminated() {
            return;
        }

        let header = self.new_block();
        let continue_blk = self.new_block();
        let body_blk = self.new_block();
        let exit = self.new_block();

        self.set_terminator(MirTerminator::Goto(header));

        self.current = header;
        let cond = self.lower_expr(condition);
        if self.is_current_terminated() {
            return;
        }
        self.set_terminator(MirTerminator::Branch { condition: cond, then_block: body_blk, else_block: exit });

        self.current = body_blk;
        self.loop_stack.push((loop_id, LoopTargets { break_block: exit, continue_block: continue_blk }));
        let fell_through = self.lower_block_as_stmt(body);
        self.loop_stack.pop();
        if fell_through {
            self.set_terminator(MirTerminator::Goto(continue_blk));
        }

        self.current = continue_blk;
        if let Some(post) = post {
            let diverges = post.r#type == ResolvedType::Never;
            let node = self.lower_expr(post);
            if !self.is_current_terminated() {
                self.push_stmt(node);
                if diverges {
                    self.set_terminator(MirTerminator::Unreachable);
                }
            }
        }
        if !self.is_current_terminated() {
            self.set_terminator(MirTerminator::Goto(header));
        }

        self.current = exit;
    }

    pub(super) fn lower_expr(&mut self, node: CheckedExprNode) -> MirExprNode {
        let CheckedExprNode { id, span, r#type, kind } = node;
        match kind {
            CheckedExpr::If(CheckedIf { branches, else_branch }) => {
                self.lower_if_expr(id, span, r#type, branches, else_branch)
            }
            CheckedExpr::Match(CheckedMatch { arms, else_branch }) => {
                self.lower_match_expr(id, span, r#type, arms, else_branch)
            }
            CheckedExpr::Codeblock(block) => self.lower_codeblock_expr(id, span, r#type, block),

            CheckedExpr::Place(place) => {
                let place = self.lower_place(place);
                MirExprNode { id, span, r#type, kind: MirExpr::Place(place) }
            }
            CheckedExpr::Number(n) => MirExprNode { id, span, r#type, kind: MirExpr::Number(n) },
            CheckedExpr::Bool(b) => MirExprNode { id, span, r#type, kind: MirExpr::Bool(b) },
            CheckedExpr::Char(c) => MirExprNode { id, span, r#type, kind: MirExpr::Char(c) },
            CheckedExpr::String(s) => MirExprNode { id, span, r#type, kind: MirExpr::String(s) },
            CheckedExpr::ByteString(s) => MirExprNode { id, span, r#type, kind: MirExpr::ByteString(s) },
            CheckedExpr::Const(v) => MirExprNode { id, span, r#type, kind: MirExpr::Const(v) },
            CheckedExpr::Sizeof(t) => MirExprNode { id, span, r#type, kind: MirExpr::Sizeof(t) },

            CheckedExpr::FunctionCall(call) => {
                let callee = Box::new(self.lower_expr(*call.callee));
                let args = call.args.into_iter().map(|a| self.lower_expr(a)).collect();
                let kind = MirExpr::FunctionCall(MirFunctionCall { callee, fn_type: call.fn_type, args });
                MirExprNode { id, span, r#type, kind }
            }
            CheckedExpr::Assignment(a) => {
                let target = self.lower_place(a.target);
                let value = Box::new(self.lower_expr(*a.value));
                MirExprNode { id, span, r#type, kind: MirExpr::Assignment(MirAssignment { target, value }) }
            }
            CheckedExpr::AddressOf(a) => {
                let place = self.lower_place(a.place);
                MirExprNode { id, span, r#type, kind: MirExpr::AddressOf(MirAddressOf { place }) }
            }
            CheckedExpr::Negate(e) => {
                let e = Box::new(self.lower_expr(*e));
                MirExprNode { id, span, r#type, kind: MirExpr::Negate(e) }
            }
            CheckedExpr::BitNot(e) => {
                let e = Box::new(self.lower_expr(*e));
                MirExprNode { id, span, r#type, kind: MirExpr::BitNot(e) }
            }
            CheckedExpr::BinaryOp(b) => {
                let left = Box::new(self.lower_expr(*b.left));
                let right = Box::new(self.lower_expr(*b.right));
                MirExprNode { id, span, r#type, kind: MirExpr::BinaryOp(MirBinaryOp { op: b.op, left, right }) }
            }
            CheckedExpr::ArrayLiteral(lit) => {
                let elements = lit.elements.into_iter().map(|e| self.lower_expr(e)).collect();
                let kind = MirExpr::ArrayLiteral(MirArrayLiteral { item_type: lit.item_type, elements });
                MirExprNode { id, span, r#type, kind }
            }
            CheckedExpr::StructLiteral(lit) => {
                let fields = lit.fields.into_iter().map(|f| self.lower_field_init(f)).collect();
                MirExprNode { id, span, r#type, kind: MirExpr::StructLiteral(MirStructLiteral { fields }) }
            }
            CheckedExpr::EnumConstruct(construct) => {
                let fields = construct.fields.into_iter().map(|f| self.lower_field_init(f)).collect();
                let kind = MirExpr::EnumConstruct(MirEnumConstruct { variant_index: construct.variant_index, fields });
                MirExprNode { id, span, r#type, kind }
            }
            CheckedExpr::UnionConstruct(construct) => {
                let value = Box::new(self.lower_expr(*construct.value));
                let kind = MirExpr::UnionConstruct(MirUnionConstruct { field_index: construct.field_index, value });
                MirExprNode { id, span, r#type, kind }
            }
            CheckedExpr::Slice(s) => {
                let base = self.lower_place(s.base);
                let start = s.start.map(|e| Box::new(self.lower_expr(*e)));
                let end = s.end.map(|e| Box::new(self.lower_expr(*e)));
                let kind = MirExpr::Slice(MirSlice { base, item_type: s.item_type, start, end, inclusive: s.inclusive });
                MirExprNode { id, span, r#type, kind }
            }
            CheckedExpr::Cast(cast) => {
                let base = Box::new(self.lower_expr(*cast.base));
                let kind = MirExpr::Cast(MirCast { kind: cast.kind, target_type: cast.target_type, base });
                MirExprNode { id, span, r#type, kind }
            }
            CheckedExpr::SpecCoerce(coerce) => {
                let base = Box::new(self.lower_expr(*coerce.base));
                let kind = MirExpr::SpecCoerce(MirSpecCoerce { base, slots: coerce.slots });
                MirExprNode { id, span, r#type, kind }
            }
            CheckedExpr::DynamicCall(call) => {
                let base = self.lower_place(call.base);
                let args = call.args.into_iter().map(|a| self.lower_expr(a)).collect();
                let kind = MirExpr::DynamicCall(MirDynamicCall {
                    base,
                    slot_index: call.slot_index,
                    fn_type: call.fn_type,
                    args,
                });
                MirExprNode { id, span, r#type, kind }
            }
        }
    }

    fn lower_field_init(&mut self, field: CheckedStructLiteralField) -> MirFieldInit {
        MirFieldInit { field_index: field.field_index, value: self.lower_expr(field.value) }
    }

    /// `if`/`match`/a bare `{ .. }` used as a value all share this shape:
    /// allocate a `result` local typed for the expression's own resolved
    /// type, lower every arm into `merge` (via a construct-specific chain
    /// helper, each arm writing its own value to `result` before jumping),
    /// and -- once done -- read `result` back as this expression's own
    /// value. See the module doc comment for how "reached" propagates and
    /// why a `false` result still leaves the block graph well-formed (an
    /// `Unreachable` merge).
    fn finish_merge(&mut self, merge: BlockId, reached: bool, result: LocalId, id: HirId, span: Span, r#type: ResolvedType) -> MirExprNode {
        self.current = merge;
        if !reached {
            self.set_terminator(MirTerminator::Unreachable);
        }
        let root = MirPlaceRoot::Local { id: result, r#type: r#type.clone() };
        let kind = MirExpr::Place(MirPlace {
            root,
            projections: vec![],
            r#type: r#type.clone(),
            align: place_align(&r#type),
        });
        MirExprNode { id, span, r#type, kind }
    }

    fn lower_codeblock_expr(&mut self, id: HirId, span: Span, r#type: ResolvedType, block: CheckedBlock) -> MirExprNode {
        let merge = self.new_block();
        let result = self.declare_local(None, r#type.clone());
        let reached = self.lower_block_into(block, merge, Some(result), r#type.clone());
        self.finish_merge(merge, reached, result, id, span, r#type)
    }

    fn lower_if_expr(
        &mut self,
        id: HirId,
        span: Span,
        r#type: ResolvedType,
        branches: Vec<(CheckedExprNode, CheckedBlock)>,
        else_branch: Option<CheckedBlock>,
    ) -> MirExprNode {
        let merge = self.new_block();
        let result = self.declare_local(None, r#type.clone());
        let reached = self.lower_if_chain(branches.into_iter(), else_branch, merge, Some(result), r#type.clone());
        self.finish_merge(merge, reached, result, id, span, r#type)
    }

    /// `if a {..} else if b {..} else {..}` recurses through `branches`
    /// exactly like `emit_if`: each arm's `then` body is lowered into
    /// `merge` directly (writing its own value to `result` first, when
    /// there is one to write -- see `lower_block_into`'s doc comment for
    /// `result: None`), and `else` is this same recursion one level
    /// deeper -- the direct analogue of `emit_if`'s own `branches:
    /// IntoIter` recursion, minus the actual `Value`/`Diverged` payload
    /// (which now just lives in `result`, when there is one).
    fn lower_if_chain(
        &mut self,
        mut branches: std::vec::IntoIter<(CheckedExprNode, CheckedBlock)>,
        else_branch: Option<CheckedBlock>,
        merge: BlockId,
        result: Option<LocalId>,
        result_type: ResolvedType,
    ) -> bool {
        let Some((cond, then_body)) = branches.next() else {
            return match else_branch {
                Some(b) => self.lower_block_into(b, merge, result, result_type),
                None => {
                    self.set_terminator(MirTerminator::Goto(merge));
                    true
                }
            };
        };

        let cond = self.lower_expr(cond);
        if self.is_current_terminated() {
            return false;
        }
        let then_blk = self.new_block();
        let else_blk = self.new_block();
        self.set_terminator(MirTerminator::Branch { condition: cond, then_block: then_blk, else_block: else_blk });

        self.current = then_blk;
        let then_reached = self.lower_block_into(then_body, merge, result, result_type.clone());
        self.current = else_blk;
        let else_reached = self.lower_if_chain(branches, else_branch, merge, result, result_type);
        then_reached || else_reached
    }

    fn lower_match_expr(
        &mut self,
        id: HirId,
        span: Span,
        r#type: ResolvedType,
        arms: Vec<CheckedMatchArm>,
        else_branch: Option<CheckedBlock>,
    ) -> MirExprNode {
        let merge = self.new_block();
        let result = self.declare_local(None, r#type.clone());
        let reached = self.lower_match_chain(arms.into_iter(), else_branch, merge, Some(result), r#type.clone());
        self.finish_merge(merge, reached, result, id, span, r#type)
    }

    /// `match`'s analogue of `lower_if_chain` -- see `emit_match`'s own doc
    /// comment for why a missing `else_branch` here means "already proved
    /// exhaustive" (traps) rather than "defaults to empty" the way `if`
    /// treats it.
    fn lower_match_chain(
        &mut self,
        mut arms: std::vec::IntoIter<CheckedMatchArm>,
        else_branch: Option<CheckedBlock>,
        merge: BlockId,
        result: Option<LocalId>,
        result_type: ResolvedType,
    ) -> bool {
        let Some(arm) = arms.next() else {
            return match else_branch {
                Some(b) => self.lower_block_into(b, merge, result, result_type),
                None => {
                    self.set_terminator(MirTerminator::Unreachable);
                    false
                }
            };
        };

        // An OR of AND-groups (see `CheckedMatchArm`'s own doc comment) --
        // any group with zero conditions is vacuously true, which makes
        // the whole arm match unconditionally regardless of any other
        // group, exactly like the old "conditions is empty" shortcut this
        // replaces.
        if arm.conditions.iter().any(|group| group.is_empty()) {
            return self.lower_block_into(arm.body, merge, result, result_type);
        }

        let body_blk = self.new_block();
        let fail_blk = self.new_block();
        let group_count = arm.conditions.len();
        let mut group_entry = self.current;
        for (g, group) in arm.conditions.into_iter().enumerate() {
            self.current = group_entry;
            // Where this group's own AND-chain falls through to once every
            // condition in it has been tried: the next group's own entry
            // block, or the arm's overall failure block once every group
            // has had its turn.
            let group_fail = if g + 1 == group_count { fail_blk } else { self.new_block() };
            let condition_count = group.len();
            for (i, cond) in group.into_iter().enumerate() {
                let cond = self.lower_expr(cond);
                if self.is_current_terminated() {
                    return false;
                }
                let true_target = if i + 1 == condition_count { body_blk } else { self.new_block() };
                self.set_terminator(MirTerminator::Branch { condition: cond, then_block: true_target, else_block: group_fail });
                self.current = true_target;
            }
            group_entry = group_fail;
        }

        self.current = body_blk;
        let body_reached = self.lower_block_into(arm.body, merge, result, result_type.clone());
        self.current = fail_blk;
        let fail_reached = self.lower_match_chain(arms, else_branch, merge, result, result_type);
        body_reached || fail_reached
    }

    // Delegates to `crate::lower::place` -- see that module's doc comment.
    fn lower_place(&mut self, place: CheckedPlace) -> MirPlace {
        super::place::lower_place(self, place)
    }
}

fn bool_literal(id: HirId, span: Span, value: bool) -> MirExprNode {
    MirExprNode { id, span, r#type: ResolvedType::Bool, kind: MirExpr::Bool(value) }
}

/// Whether `kind` is one of the three shapes that lower to a
/// [`crate::body::MirTerminator`]-based control-flow graph instead of an
/// ordinary [`crate::body::MirExprNode`] tree -- the guard every fast path
/// in this file (`lower_expr_stmt`'s two, `lower_stmt`'s `Return` arm,
/// `lower_function_body`'s tail) checks before routing into
/// `FunctionLowerer::lower_control_flow_into`/`lower_control_flow_stmt`
/// instead of the general, always-safe-but-always-allocates-a-temp
/// `lower_expr`.
fn is_control_flow_expr(kind: &CheckedExpr) -> bool {
    matches!(kind, CheckedExpr::If(_) | CheckedExpr::Match(_) | CheckedExpr::Codeblock(_))
}

/// Every `defer` in `block`, depth-first in source order, with the span its
/// own statement covers -- the pre-pass `FunctionLowerer::lower` needs
/// before the real walk begins (see its doc comment). Direct copy of
/// `omega_codegen`'s former `collect_defer_ids*` family, which walked the
/// identical `CheckedBlock`/`CheckedStmt`/`CheckedExprNode`/`CheckedPlace`
/// shapes for the identical reason.
fn collect_defer_ids(block: &CheckedBlock, out: &mut Vec<(HirId, Span)>) {
    for stmt in &block.stmts {
        collect_defer_ids_stmt(stmt, out);
    }
    if let Some(tail) = &block.tail {
        collect_defer_ids_expr(tail, out);
    }
}

fn collect_defer_ids_stmt(stmt: &CheckedStmt, out: &mut Vec<(HirId, Span)>) {
    match stmt {
        CheckedStmt::Declaration(_) | CheckedStmt::ExternDeclaration(_) | CheckedStmt::Break(_)
        | CheckedStmt::Continue(_) => {}
        CheckedStmt::Expression(e) | CheckedStmt::Return(e) => collect_defer_ids_expr(e, out),
        CheckedStmt::While(w) => {
            collect_defer_ids_expr(&w.condition, out);
            collect_defer_ids(&w.body, out);
        }
        CheckedStmt::Loop(l) => collect_defer_ids(&l.body, out),
        CheckedStmt::For(f) => {
            for s in &f.init {
                collect_defer_ids_stmt(s, out);
            }
            collect_defer_ids_expr(&f.condition, out);
            if let Some(post) = &f.post {
                collect_defer_ids_expr(post, out);
            }
            collect_defer_ids(&f.body, out);
        }
        CheckedStmt::Defer(d) => {
            out.push((d.id, d.span));
            // Always empty in practice -- analysis rejects a nested `defer`
            // -- but walked anyway for uniformity, same as the original.
            collect_defer_ids(&d.body, out);
        }
    }
}

fn collect_defer_ids_expr(expr: &CheckedExprNode, out: &mut Vec<(HirId, Span)>) {
    match &expr.kind {
        CheckedExpr::Number(_)
        | CheckedExpr::Bool(_)
        | CheckedExpr::Char(_)
        | CheckedExpr::String(_)
        | CheckedExpr::ByteString(_)
        | CheckedExpr::Const(_)
        | CheckedExpr::Sizeof(_) => {}
        CheckedExpr::Place(p) => collect_defer_ids_place(p, out),
        CheckedExpr::FunctionCall(call) => {
            collect_defer_ids_expr(&call.callee, out);
            for arg in &call.args {
                collect_defer_ids_expr(arg, out);
            }
        }
        CheckedExpr::Assignment(a) => {
            collect_defer_ids_place(&a.target, out);
            collect_defer_ids_expr(&a.value, out);
        }
        CheckedExpr::AddressOf(a) => collect_defer_ids_place(&a.place, out),
        CheckedExpr::Negate(e) => collect_defer_ids_expr(e, out),
        CheckedExpr::BitNot(e) => collect_defer_ids_expr(e, out),
        CheckedExpr::BinaryOp(b) => {
            collect_defer_ids_expr(&b.left, out);
            collect_defer_ids_expr(&b.right, out);
        }
        CheckedExpr::Codeblock(block) => collect_defer_ids(block, out),
        CheckedExpr::If(if_expr) => {
            for (cond, block) in &if_expr.branches {
                collect_defer_ids_expr(cond, out);
                collect_defer_ids(block, out);
            }
            if let Some(else_branch) = &if_expr.else_branch {
                collect_defer_ids(else_branch, out);
            }
        }
        CheckedExpr::ArrayLiteral(lit) => {
            for e in &lit.elements {
                collect_defer_ids_expr(e, out);
            }
        }
        CheckedExpr::StructLiteral(lit) => {
            for f in &lit.fields {
                collect_defer_ids_expr(&f.value, out);
            }
        }
        CheckedExpr::EnumConstruct(construct) => {
            for f in &construct.fields {
                collect_defer_ids_expr(&f.value, out);
            }
        }
        CheckedExpr::Slice(s) => {
            collect_defer_ids_place(&s.base, out);
            if let Some(start) = &s.start {
                collect_defer_ids_expr(start, out);
            }
            if let Some(end) = &s.end {
                collect_defer_ids_expr(end, out);
            }
        }
        CheckedExpr::Match(m) => {
            for arm in &m.arms {
                for group in &arm.conditions {
                    for cond in group {
                        collect_defer_ids_expr(cond, out);
                    }
                }
                collect_defer_ids(&arm.body, out);
            }
            if let Some(else_branch) = &m.else_branch {
                collect_defer_ids(else_branch, out);
            }
        }
        CheckedExpr::Cast(cast) => collect_defer_ids_expr(&cast.base, out),
        CheckedExpr::UnionConstruct(construct) => collect_defer_ids_expr(&construct.value, out),
        CheckedExpr::SpecCoerce(coerce) => collect_defer_ids_expr(&coerce.base, out),
        CheckedExpr::DynamicCall(call) => {
            collect_defer_ids_place(&call.base, out);
            for arg in &call.args {
                collect_defer_ids_expr(arg, out);
            }
        }
    }
}

fn collect_defer_ids_place(place: &CheckedPlace, out: &mut Vec<(HirId, Span)>) {
    if let CheckedPlaceRoot::Expr(e) = &place.root {
        collect_defer_ids_expr(e, out);
    }
    for proj in &place.projections {
        if let CheckedProjection::Index { index_expr, .. } = proj {
            collect_defer_ids_expr(index_expr, out);
        }
    }
}
