use super::Lowerer;
use crate::hir::{
    HirAddressOf, HirAssignment, HirBinaryOp, HirCast, HirCompoundAssign, HirExpr, HirExprNode,
    HirFunctionCall, HirIf, HirLogical, HirMatch, HirMatchArm, HirPattern, HirPatternValue,
    HirPlace, HirPlaceRoot, HirProjection, HirRange, HirRangeEnd, HirReveal, HirSlice,
    HirStructLiteral, HirStructLiteralField, HirTry,
};
use omega_parser::prelude::{
    Expression, ExpressionNode, Pattern, PatternValue, RangeEnd, RangeExpr, Span,
};

impl Lowerer {
    fn node(&mut self, span: Span, expr: HirExpr) -> HirExprNode {
        HirExprNode {
            id: self.ids.next(),
            span,
            expr,
        }
    }

    fn lower_unary(
        &mut self,
        span: Span,
        base: &ExpressionNode,
        constructor: fn(Box<HirExprNode>) -> HirExpr,
    ) -> HirExprNode {
        let base = Box::new(self.lower_expr(base));
        self.node(span, constructor(base))
    }

    pub(super) fn lower_expr(&mut self, node: &ExpressionNode) -> HirExprNode {
        match &node.expression {
            Expression::Path(_)
            | Expression::FieldAccess(_)
            | Expression::Index(_)
            | Expression::Deref(_) => {
                let place = self.lower_place_chain(node);
                self.node(node.span, HirExpr::Place(place))
            }
            Expression::Number(n) => self.node(node.span, HirExpr::Number(n.clone())),
            Expression::String(s) => self.node(node.span, HirExpr::String(s.clone())),
            Expression::ByteString(s) => self.node(node.span, HirExpr::ByteString(s.clone())),
            Expression::Bool(b) => self.node(node.span, HirExpr::Bool(b.0)),
            Expression::Char(c) => self.node(node.span, HirExpr::Char(c.0)),
            Expression::Codeblock(cb) => {
                let block = self.lower_block(cb);
                self.node(node.span, HirExpr::Codeblock(block))
            }
            Expression::If(if_expr) => {
                let branches = if_expr
                    .branches
                    .iter()
                    .map(|(cond, block)| (self.lower_expr(cond), self.lower_block(block)))
                    .collect();
                let else_branch = if_expr.else_branch.as_ref().map(|b| self.lower_block(b));
                self.node(
                    node.span,
                    HirExpr::If(HirIf {
                        branches,
                        else_branch,
                    }),
                )
            }
            Expression::FunctionCall(call) => {
                let callee = Box::new(self.lower_expr(&call.callee));
                let args = call.args.iter().map(|a| self.lower_expr(a)).collect();
                self.node(
                    node.span,
                    HirExpr::FunctionCall(HirFunctionCall { callee, args }),
                )
            }
            Expression::Assignment(assign) => {
                let target = Box::new(self.lower_expr(&assign.target));
                let value = Box::new(self.lower_expr(&assign.value));
                self.node(
                    node.span,
                    HirExpr::Assignment(HirAssignment { target, value }),
                )
            }
            Expression::CompoundAssign(assign) => {
                let target = Box::new(self.lower_expr(&assign.target));
                let value = Box::new(self.lower_expr(&assign.value));
                self.node(
                    node.span,
                    HirExpr::CompoundAssign(HirCompoundAssign {
                        target,
                        op: assign.op,
                        value,
                    }),
                )
            }
            Expression::AddressOf(addr) => {
                let base = Box::new(self.lower_expr(&addr.base));
                self.node(
                    node.span,
                    HirExpr::AddressOf(HirAddressOf {
                        base,
                        mutable: addr.mutable,
                    }),
                )
            }
            Expression::Reveal(reveal) => {
                let base = Box::new(self.lower_expr(&reveal.base));
                self.node(
                    node.span,
                    HirExpr::Reveal(HirReveal {
                        base,
                        origin: reveal.origin,
                    }),
                )
            }
            Expression::Comp(comp) => self.lower_unary(node.span, &comp.base, HirExpr::Comp),
            Expression::Negate(negate) => {
                self.lower_unary(node.span, &negate.base, HirExpr::Negate)
            }
            Expression::BitNot(bit_not) => {
                self.lower_unary(node.span, &bit_not.base, HirExpr::BitNot)
            }
            Expression::Not(not) => self.lower_unary(node.span, &not.base, HirExpr::Not),
            Expression::Logical(logical) => {
                let left = Box::new(self.lower_expr(&logical.left));
                let right = Box::new(self.lower_expr(&logical.right));
                self.node(
                    node.span,
                    HirExpr::Logical(HirLogical {
                        op: logical.op,
                        left,
                        right,
                    }),
                )
            }
            Expression::Cast(cast) => {
                let base = Box::new(self.lower_expr(&cast.base));
                self.node(
                    node.span,
                    HirExpr::Cast(HirCast {
                        target: cast.target.clone(),
                        base,
                    }),
                )
            }
            Expression::Sizeof(sizeof) => {
                self.node(node.span, HirExpr::Sizeof(sizeof.r#type.clone()))
            }
            Expression::Increment(increment) => {
                self.lower_unary(node.span, &increment.base, HirExpr::Increment)
            }
            Expression::Decrement(decrement) => {
                self.lower_unary(node.span, &decrement.base, HirExpr::Decrement)
            }
            Expression::BinaryOp(bin) => {
                let left = Box::new(self.lower_expr(&bin.left));
                let right = Box::new(self.lower_expr(&bin.right));
                self.node(
                    node.span,
                    HirExpr::BinaryOp(HirBinaryOp {
                        op: bin.op,
                        left,
                        right,
                    }),
                )
            }
            Expression::ArrayLiteral(lit) => {
                let elements = lit.elements.iter().map(|e| self.lower_expr(e)).collect();
                self.node(node.span, HirExpr::ArrayLiteral(elements))
            }
            Expression::StructLiteral(lit) => {
                let fields = lit
                    .fields
                    .iter()
                    .map(|f| HirStructLiteralField {
                        name: f.name.clone(),
                        name_span: f.name_span,
                        name_origin: f.name_origin,
                        value: self.lower_expr(&f.value),
                    })
                    .collect();
                self.node(
                    node.span,
                    HirExpr::StructLiteral(HirStructLiteral {
                        path: lit.path.clone(),
                        fields,
                    }),
                )
            }
            Expression::Slice(s) => {
                let base = self.lower_place_chain(&s.base);
                let range = self.lower_range(&s.range);
                self.node(node.span, HirExpr::Slice(HirSlice { base, range }))
            }
            Expression::Range(r) => {
                let range = self.lower_range(r);
                self.node(node.span, HirExpr::Range(range))
            }
            Expression::Match(m) => {
                let scrutinee = Box::new(self.lower_expr(&m.scrutinee));
                let arms = m
                    .arms
                    .iter()
                    .map(|arm| HirMatchArm {
                        pattern: self.lower_pattern(&arm.pattern),
                        body: self.lower_expr(&arm.body),
                        span: arm.span,
                    })
                    .collect();
                let else_branch = m.else_branch.as_ref().map(|b| self.lower_block(b));
                self.node(
                    node.span,
                    HirExpr::Match(HirMatch {
                        scrutinee,
                        arms,
                        else_branch,
                    }),
                )
            }
            Expression::Try(t) => {
                let base = Box::new(self.lower_expr(&t.base));
                self.node(
                    node.span,
                    HirExpr::Try(HirTry {
                        base,
                        operator_span: t.operator_span,
                    }),
                )
            }
            Expression::MacroInvocation(_) => unreachable!(
                "macro invocations are replaced by their expansion by \
                 omega_parser::macros::expand before lower_module runs"
            ),
        }
    }

    fn lower_range(&mut self, range: &RangeExpr) -> HirRange {
        let end = match &range.end {
            RangeEnd::Inclusive(e) => HirRangeEnd::Inclusive(Box::new(self.lower_expr(e))),
            RangeEnd::Exclusive(e) => HirRangeEnd::Exclusive(Box::new(self.lower_expr(e))),
            RangeEnd::Open => HirRangeEnd::Open,
        };
        HirRange {
            start: range.start.as_ref().map(|e| Box::new(self.lower_expr(e))),
            end,
            span: range.span,
        }
    }

    fn lower_pattern(&mut self, pattern: &Pattern) -> HirPattern {
        HirPattern {
            value: pattern.value.as_ref().map(|value| match value {
                PatternValue::Value(v) => HirPatternValue::Value(self.lower_expr(v)),
                PatternValue::Range(r) => HirPatternValue::Range(self.lower_range(r)),
            }),
            r#type: pattern.r#type.clone(),
            span: pattern.span,
        }
    }

    fn lower_place_chain(&mut self, expr: &ExpressionNode) -> HirPlace {
        match &expr.expression {
            Expression::Path(path) => HirPlace {
                root: HirPlaceRoot::Path(path.clone()),
                projections: vec![],
            },
            Expression::FieldAccess(access) => {
                let mut place = self.lower_place_chain(&access.base);
                place.projections.push(HirProjection::FieldAccess(
                    access.field.clone(),
                    access.field_origin,
                ));
                place
            }
            Expression::Index(index_expr) => {
                let mut place = self.lower_place_chain(&index_expr.base);
                let index = Box::new(self.lower_expr(&index_expr.index));
                place.projections.push(HirProjection::Index(index));
                place
            }
            Expression::Deref(deref) => {
                let mut place = self.lower_place_chain(&deref.base);
                place.projections.push(HirProjection::Deref);
                place
            }
            // Base isn't syntactically a place (e.g. `foo().bar`) -- root is
            // just the lowered expression itself.
            _ => HirPlace {
                root: HirPlaceRoot::Expr(Box::new(self.lower_expr(expr))),
                projections: vec![],
            },
        }
    }
}
