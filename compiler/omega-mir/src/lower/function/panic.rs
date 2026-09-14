use super::FunctionLowerer;
use crate::body::{
    MirAddressOf, MirExpr, MirExprNode, MirFieldInit, MirFunctionCall, MirPlace, MirPlaceRoot,
    MirStructLiteral, MirTerminator,
};
use crate::lower::place::place_align;
use omega_analyzer::checked::{CheckedExprNode, NumberValue};
use omega_analyzer::resolved_type::ResolvedType;
use omega_analyzer::runtime_checks::{PanicInfoField, RuntimeCheck, static_callee};
use omega_hir::HirId;
use omega_parser::prelude::Span;

impl FunctionLowerer {
    /// Whether a call names the canonical `core::panic::PanicHandler::panic`
    /// gap function itself, by the declaration analysis resolved rather than
    /// by any spelling a program could choose for a function of its own.
    pub(super) fn is_trusted_panic_handler(&self, callee: &CheckedExprNode) -> bool {
        let Some(checks) = &self.runtime_checks else {
            return false;
        };
        static_callee(callee) == Some(checks.support().handler_decl_id)
    }

    /// Reports a violated runtime invariant through `core`'s panic gap and
    /// ends the block. The handler is the trusted terminal operation, so the
    /// continuation after its call is a structural `Unreachable` rather than
    /// one more guarded call.
    ///
    /// Everything here is ordinary MIR -- a stack-local struct, its address,
    /// and a call -- so this path allocates nothing, formats nothing, and
    /// needs no platform-specific lowering.
    pub(super) fn emit_runtime_panic(&mut self, id: HirId, span: Span, check: RuntimeCheck) {
        let checks = self.runtime_checks.clone().expect(
            "omega-mir lowering bug: a body reached a runtime check the analyzer found no site for",
        );
        let location = checks.location(span);
        let support = checks.support();

        let field = |field: &PanicInfoField, kind: MirExpr| MirFieldInit {
            field_index: field.index,
            value: MirExprNode {
                id,
                span,
                r#type: field.r#type.clone(),
                kind,
            },
        };
        let info = MirExprNode {
            id,
            span,
            r#type: support.info_type.clone(),
            kind: MirExpr::StructLiteral(MirStructLiteral {
                fields: vec![
                    field(&support.source_file, MirExpr::String(location.file)),
                    field(
                        &support.line,
                        MirExpr::Number(NumberValue::Unsigned(location.line as u64)),
                    ),
                    field(
                        &support.column,
                        MirExpr::Number(NumberValue::Unsigned(location.column as u64)),
                    ),
                    field(
                        &support.message,
                        MirExpr::String(check.message().to_string()),
                    ),
                ],
            }),
        };
        let fn_type = support.handler_fn_type.clone();
        let info_pointer = fn_type.params[0].r#type.clone();
        let handler = support.handler_decl_id;

        let slot = self.materialize_place(info);
        let callee_type = ResolvedType::Function(fn_type.clone());
        let callee = MirExprNode {
            id,
            span,
            r#type: callee_type.clone(),
            kind: MirExpr::Place(MirPlace {
                root: MirPlaceRoot::Function(handler),
                projections: Vec::new(),
                align: place_align(&callee_type),
                r#type: callee_type,
            }),
        };
        let argument = MirExprNode {
            id,
            span,
            r#type: info_pointer,
            kind: MirExpr::AddressOf(MirAddressOf { place: slot }),
        };
        let call = MirExprNode {
            id,
            span,
            r#type: ResolvedType::Never,
            kind: MirExpr::FunctionCall(MirFunctionCall {
                callee: Box::new(callee),
                fn_type,
                args: vec![argument],
            }),
        };
        self.push_stmt(call);
        self.terminate(MirTerminator::Unreachable);
    }
}
