use super::*;
use crate::checked::{
    CheckedAssignment, CheckedIf, CheckedParam, CheckedStructLiteral, CheckedStructLiteralField,
    CheckedWhile,
};
use omega_hir::ModuleId;

fn id(n: u32) -> HirId {
    HirId {
        module: ModuleId(0),
        local: n,
    }
}

fn sp() -> Span {
    Span::new(0, 0)
}

fn node(kind: CheckedExpr, r#type: ResolvedType) -> CheckedExprNode {
    CheckedExprNode {
        id: id(9999),
        span: sp(),
        r#type,
        kind,
    }
}

fn num(n: i64) -> CheckedExprNode {
    node(
        CheckedExpr::Number(NumberValue::Signed(n)),
        ResolvedType::I32,
    )
}

fn local_place(decl: HirId, r#type: ResolvedType) -> CheckedPlace {
    CheckedPlace {
        root: CheckedPlaceRoot::Variable {
            decl_id: decl,
            storage: Storage::Local,
            r#type: r#type.clone(),
        },
        projections: vec![],
        r#type,
    }
}

struct NoFunctions;
impl CompFunctionResolver for NoFunctions {
    fn resolve_function_body(
        &mut self,
        _decl_id: HirId,
    ) -> Result<Option<CheckedFunctionDef>, ResolveError> {
        panic!("this test never calls a function")
    }
}

#[test]
fn arithmetic_folds() {
    let expr = node(
        CheckedExpr::BinaryOp(CheckedBinaryOp {
            op: BinaryOp::Add,
            left: Box::new(num(10)),
            right: Box::new(num(20)),
        }),
        ResolvedType::I32,
    );
    let value = eval(&mut NoFunctions, &expr, Target::DEFAULT, None).unwrap();
    assert_eq!(value, ConstValue::Number(NumberValue::Signed(30)));
}

#[test]
fn division_by_zero_is_rejected_not_a_panic() {
    let expr = node(
        CheckedExpr::BinaryOp(CheckedBinaryOp {
            op: BinaryOp::Div,
            left: Box::new(num(1)),
            right: Box::new(num(0)),
        }),
        ResolvedType::I32,
    );
    let err = eval(&mut NoFunctions, &expr, Target::DEFAULT, None).unwrap_err();
    assert!(matches!(err.kind, CompErrorKind::Unsupported(_)));
}

#[test]
fn if_else_picks_the_taken_branch() {
    let cond = node(CheckedExpr::Bool(false), ResolvedType::Bool);
    let then_block = CheckedBlock {
        stmts: vec![],
        tail: Some(Box::new(num(1))),
    };
    let else_block = CheckedBlock {
        stmts: vec![],
        tail: Some(Box::new(num(2))),
    };
    let expr = node(
        CheckedExpr::If(CheckedIf {
            branches: vec![(cond, then_block)],
            else_branch: Some(else_block),
        }),
        ResolvedType::I32,
    );
    let value = eval(&mut NoFunctions, &expr, Target::DEFAULT, None).unwrap();
    assert_eq!(value, ConstValue::Number(NumberValue::Signed(2)));
}

#[test]
fn struct_literal_builds_fields_in_declared_order() {
    let struct_ty = ResolvedType::Bool; // placeholder -- struct fields don't need a real ResolvedStructType for this test
    let lit = CheckedStructLiteral {
        fields: vec![
            CheckedStructLiteralField {
                field_index: 1,
                value: num(20),
            },
            CheckedStructLiteralField {
                field_index: 0,
                value: num(10),
            },
        ],
    };
    let expr = node(CheckedExpr::StructLiteral(lit), struct_ty);
    let value = eval(&mut NoFunctions, &expr, Target::DEFAULT, None).unwrap();
    assert_eq!(
        value,
        ConstValue::Struct(vec![
            ConstValue::Number(NumberValue::Signed(10)),
            ConstValue::Number(NumberValue::Signed(20))
        ])
    );
}

#[test]
fn while_loop_accumulates_via_locals() {
    let i_id = id(1);
    let sum_id = id(2);
    let i_place = local_place(i_id, ResolvedType::I32);
    let sum_place = local_place(sum_id, ResolvedType::I32);

    let cond = node(
        CheckedExpr::BinaryOp(CheckedBinaryOp {
            op: BinaryOp::Lt,
            left: Box::new(node(CheckedExpr::Place(i_place.clone()), ResolvedType::I32)),
            right: Box::new(num(5)),
        }),
        ResolvedType::Bool,
    );
    let sum_incr = CheckedStmt::Expression(node(
        CheckedExpr::Assignment(CheckedAssignment {
            target: sum_place.clone(),
            value: Box::new(node(
                CheckedExpr::BinaryOp(CheckedBinaryOp {
                    op: BinaryOp::Add,
                    left: Box::new(node(
                        CheckedExpr::Place(sum_place.clone()),
                        ResolvedType::I32,
                    )),
                    right: Box::new(node(CheckedExpr::Place(i_place.clone()), ResolvedType::I32)),
                }),
                ResolvedType::I32,
            )),
        }),
        ResolvedType::I32,
    ));
    let i_incr = CheckedStmt::Expression(node(
        CheckedExpr::Assignment(CheckedAssignment {
            target: i_place.clone(),
            value: Box::new(node(
                CheckedExpr::BinaryOp(CheckedBinaryOp {
                    op: BinaryOp::Add,
                    left: Box::new(node(CheckedExpr::Place(i_place.clone()), ResolvedType::I32)),
                    right: Box::new(num(1)),
                }),
                ResolvedType::I32,
            )),
        }),
        ResolvedType::I32,
    ));
    let body = CheckedBlock {
        stmts: vec![sum_incr, i_incr],
        tail: None,
    };
    let while_stmt = CheckedStmt::While(CheckedWhile {
        id: id(3),
        span: sp(),
        condition: cond,
        body,
    });

    let init_i = CheckedStmt::Expression(node(
        CheckedExpr::Assignment(CheckedAssignment {
            target: i_place.clone(),
            value: Box::new(num(0)),
        }),
        ResolvedType::I32,
    ));
    let init_sum = CheckedStmt::Expression(node(
        CheckedExpr::Assignment(CheckedAssignment {
            target: sum_place.clone(),
            value: Box::new(num(0)),
        }),
        ResolvedType::I32,
    ));

    let outer = CheckedBlock {
        stmts: vec![init_i, init_sum, while_stmt],
        tail: Some(Box::new(node(
            CheckedExpr::Place(sum_place),
            ResolvedType::I32,
        ))),
    };
    let expr = node(CheckedExpr::Codeblock(outer), ResolvedType::I32);

    let value = eval(&mut NoFunctions, &expr, Target::DEFAULT, None).unwrap();
    assert_eq!(
        value,
        ConstValue::Number(NumberValue::Signed(0 + 1 + 2 + 3 + 4))
    );
}

#[test]
fn infinite_loop_exhausts_fuel_instead_of_hanging() {
    let cond = node(CheckedExpr::Bool(true), ResolvedType::Bool);
    let body = CheckedBlock {
        stmts: vec![],
        tail: None,
    };
    let while_stmt = CheckedStmt::While(CheckedWhile {
        id: id(1),
        span: sp(),
        condition: cond,
        body,
    });
    let outer = CheckedBlock {
        stmts: vec![while_stmt],
        tail: Some(Box::new(num(0))),
    };
    let expr = node(CheckedExpr::Codeblock(outer), ResolvedType::I32);

    let err = eval(&mut NoFunctions, &expr, Target::DEFAULT, None).unwrap_err();
    assert!(matches!(err.kind, CompErrorKind::FuelExhausted));
}

#[test]
fn calling_an_extern_is_rejected_with_a_precise_reason() {
    struct AllExtern;
    impl CompFunctionResolver for AllExtern {
        fn resolve_function_body(
            &mut self,
            _decl_id: HirId,
        ) -> Result<Option<CheckedFunctionDef>, ResolveError> {
            Ok(None)
        }
    }
    let callee = node(
        CheckedExpr::Place(CheckedPlace {
            root: CheckedPlaceRoot::Variable {
                decl_id: id(42),
                storage: Storage::Function,
                r#type: ResolvedType::Function(crate::resolved_type::ResolvedFunctionType {
                    params: vec![],
                    return_type: Box::new(ResolvedType::Void),
                    is_variadic: false,
                    self_mode: None,
                    calling_convention: crate::resolved_type::CallingConvention::Omega,
                }),
            },
            projections: vec![],
            r#type: ResolvedType::Function(crate::resolved_type::ResolvedFunctionType {
                params: vec![],
                return_type: Box::new(ResolvedType::Void),
                is_variadic: false,
                self_mode: None,
                calling_convention: crate::resolved_type::CallingConvention::Omega,
            }),
        }),
        ResolvedType::Function(crate::resolved_type::ResolvedFunctionType {
            params: vec![],
            return_type: Box::new(ResolvedType::Void),
            is_variadic: false,
            self_mode: None,
            calling_convention: crate::resolved_type::CallingConvention::Omega,
        }),
    );
    let call = node(
        CheckedExpr::FunctionCall(CheckedFunctionCall {
            callee: Box::new(callee),
            fn_type: crate::resolved_type::ResolvedFunctionType {
                params: vec![],
                return_type: Box::new(ResolvedType::Void),
                is_variadic: false,
                self_mode: None,
                calling_convention: crate::resolved_type::CallingConvention::Omega,
            },
            args: vec![],
        }),
        ResolvedType::Void,
    );

    let err = eval(&mut AllExtern, &call, Target::DEFAULT, None).unwrap_err();
    assert!(matches!(err.kind, CompErrorKind::ExternCall));
}

#[test]
fn calling_another_function_interprets_its_own_body() {
    let a_id = id(1);
    let b_id = id(2);
    let add_body = CheckedBlock {
        stmts: vec![],
        tail: Some(Box::new(node(
            CheckedExpr::BinaryOp(CheckedBinaryOp {
                op: BinaryOp::Add,
                left: Box::new(node(
                    CheckedExpr::Place(local_place(a_id, ResolvedType::I32)),
                    ResolvedType::I32,
                )),
                right: Box::new(node(
                    CheckedExpr::Place(local_place(b_id, ResolvedType::I32)),
                    ResolvedType::I32,
                )),
            }),
            ResolvedType::I32,
        ))),
    };
    let add_def = CheckedFunctionDef {
        id: id(100),
        span: sp(),
        name: omega_parser::prelude::Ident("add".into()),
        type_args: vec![],
        self_mode: None,
        is_variadic: false,
        params: vec![
            CheckedParam {
                id: a_id,
                span: sp(),
                ident: omega_parser::prelude::Ident("a".into()),
                r#type: ResolvedType::I32,
            },
            CheckedParam {
                id: b_id,
                span: sp(),
                ident: omega_parser::prelude::Ident("b".into()),
                r#type: ResolvedType::I32,
            },
        ],
        return_type: ResolvedType::I32,
        body: add_body,
        inline: None,
        mangling: crate::annotations::ManglingMode::Enabled,
        conformance_owner: None,
        primitive_target: None,
        naked: false,
    };

    struct OneFunction(CheckedFunctionDef);
    impl CompFunctionResolver for OneFunction {
        fn resolve_function_body(
            &mut self,
            decl_id: HirId,
        ) -> Result<Option<CheckedFunctionDef>, ResolveError> {
            if decl_id == self.0.id {
                Ok(Some(self.0.clone()))
            } else {
                Ok(None)
            }
        }
    }

    let fn_type = crate::resolved_type::ResolvedFunctionType {
        params: vec![
            crate::resolved_type::ResolvedFunctionParam::described(
                omega_parser::prelude::Ident("a".into()),
                ResolvedType::I32,
            ),
            crate::resolved_type::ResolvedFunctionParam::described(
                omega_parser::prelude::Ident("b".into()),
                ResolvedType::I32,
            ),
        ],
        return_type: Box::new(ResolvedType::I32),
        is_variadic: false,
        self_mode: None,
        calling_convention: crate::resolved_type::CallingConvention::Omega,
    };
    let callee = node(
        CheckedExpr::Place(CheckedPlace {
            root: CheckedPlaceRoot::Variable {
                decl_id: id(100),
                storage: Storage::Function,
                r#type: ResolvedType::Function(fn_type.clone()),
            },
            projections: vec![],
            r#type: ResolvedType::Function(fn_type.clone()),
        }),
        ResolvedType::Function(fn_type.clone()),
    );
    let call = node(
        CheckedExpr::FunctionCall(CheckedFunctionCall {
            callee: Box::new(callee),
            fn_type,
            args: vec![num(10), num(20)],
        }),
        ResolvedType::I32,
    );

    let mut resolver = OneFunction(add_def);
    let value = eval(&mut resolver, &call, Target::DEFAULT, None).unwrap();
    assert_eq!(value, ConstValue::Number(NumberValue::Signed(30)));
}

/// A two-variant fallible enum shaped like `core`'s, built directly so the
/// evaluator tests need no module resolution.
fn fallible_enum(
    local: u32,
    name: &str,
    success: &str,
    failure: &str,
    success_type: ResolvedType,
    error_type: Option<ResolvedType>,
) -> ResolvedType {
    use crate::resolved_type::{ResolvedEnumType, ResolvedEnumVariant, ResolvedField};
    use omega_parser::prelude::{Ident, Visibility};
    let field = |name: &str, r#type: ResolvedType| {
        ResolvedField::new(Ident(name.to_string()), r#type, Visibility::Exposed)
    };
    ResolvedType::Enum {
        cell: std::rc::Rc::new(std::cell::RefCell::new(ResolvedEnumType {
            id: id(local),
            name: Ident(name.to_string()),
            module_path: vec![Ident("core".into())],
            type_args: vec![],
            tag_type: ResolvedType::U8,
            header: vec![],
            dynamic_fields: vec![],
            variants: vec![
                ResolvedEnumVariant {
                    name: Ident(success.to_string()),
                    tag: NumberValue::Unsigned(0),
                    header_values: vec![],
                    fields: vec![field("value", success_type)],
                },
                ResolvedEnumVariant {
                    name: Ident(failure.to_string()),
                    tag: NumberValue::Unsigned(1),
                    header_values: vec![],
                    fields: error_type.into_iter().map(|t| field("error", t)).collect(),
                },
            ],
            functions: vec![],
            layout: crate::annotations::Layout::default(),
            suppress: vec![],
        })),
        variant: None,
    }
}

fn fallible_const(variant_index: usize, payload: Vec<ConstValue>) -> ConstValue {
    ConstValue::Enum {
        variant_index,
        tag: NumberValue::Unsigned(variant_index as u64),
        header: vec![],
        dynamic_fields: vec![],
        fields: payload,
    }
}

/// `operand?` in a function returning `destination_type`, with the operand
/// supplied as an already-evaluated constant.
fn try_node(
    operand: ConstValue,
    operand_type: ResolvedType,
    destination_type: ResolvedType,
    success_type: ResolvedType,
    carries_error: bool,
    error_coercion: CheckedCoercion,
) -> CheckedExprNode {
    let payload = carries_error.then_some((0, ResolvedType::I32));
    node(
        CheckedExpr::Try(CheckedTry {
            operand: Box::new(node(CheckedExpr::Const(operand), operand_type)),
            operator_span: sp(),
            kind: if carries_error {
                crate::checked::CheckedTryKind::Result
            } else {
                crate::checked::CheckedTryKind::Option
            },
            source: crate::checked::CheckedTrySource {
                tag_type: ResolvedType::U8,
                success_variant: 0,
                success_tag: NumberValue::Unsigned(0),
                success_field: 0,
                failure_variant: 1,
                failure_payload: payload.clone(),
            },
            destination: crate::checked::CheckedTryDestination {
                r#type: destination_type,
                failure_variant: 1,
                failure_field: payload.map(|(field, _)| field),
                error_coercion,
            },
        }),
        success_type,
    )
}

/// Wraps `body` in a called function so a failing `?` has a frame to return
/// from, mirroring how the operator is always used in real source.
fn call_returning(body: CheckedBlock, return_type: ResolvedType) -> (CheckedExprNode, TheCallee) {
    let def = CheckedFunctionDef {
        id: id(100),
        span: sp(),
        name: omega_parser::prelude::Ident("callee".into()),
        type_args: vec![],
        self_mode: None,
        is_variadic: false,
        params: vec![],
        return_type: return_type.clone(),
        body,
        inline: None,
        mangling: crate::annotations::ManglingMode::Enabled,
        conformance_owner: None,
        primitive_target: None,
        naked: false,
    };
    let fn_type = crate::resolved_type::ResolvedFunctionType {
        params: vec![],
        return_type: Box::new(return_type.clone()),
        is_variadic: false,
        self_mode: None,
        calling_convention: crate::resolved_type::CallingConvention::Omega,
    };
    let callee = node(
        CheckedExpr::Place(CheckedPlace {
            root: CheckedPlaceRoot::Variable {
                decl_id: def.id,
                storage: Storage::Function,
                r#type: ResolvedType::Function(fn_type.clone()),
            },
            projections: vec![],
            r#type: ResolvedType::Function(fn_type.clone()),
        }),
        ResolvedType::Function(fn_type.clone()),
    );
    let call = node(
        CheckedExpr::FunctionCall(CheckedFunctionCall {
            callee: Box::new(callee),
            fn_type,
            args: vec![],
        }),
        return_type,
    );
    (call, TheCallee(def))
}

struct TheCallee(CheckedFunctionDef);
impl CompFunctionResolver for TheCallee {
    fn resolve_function_body(
        &mut self,
        decl_id: HirId,
    ) -> Result<Option<CheckedFunctionDef>, ResolveError> {
        Ok((decl_id == self.0.id).then(|| self.0.clone()))
    }
}

#[test]
fn a_successful_try_yields_the_payload() {
    let option = fallible_enum(1, "Option", "Some", "None", ResolvedType::I32, None);
    let expr = try_node(
        fallible_const(0, vec![ConstValue::Number(NumberValue::Signed(7))]),
        option.clone(),
        option,
        ResolvedType::I32,
        false,
        CheckedCoercion::default(),
    );
    let (call, mut resolver) = call_returning(
        CheckedBlock {
            stmts: vec![],
            tail: Some(Box::new(expr)),
        },
        ResolvedType::I32,
    );
    let value = eval(&mut resolver, &call, Target::DEFAULT, None).unwrap();
    assert_eq!(value, ConstValue::Number(NumberValue::Signed(7)));
}

#[test]
fn a_failing_option_try_returns_the_enclosing_none() {
    let option = fallible_enum(1, "Option", "Some", "None", ResolvedType::I32, None);
    let expr = try_node(
        fallible_const(1, vec![]),
        option.clone(),
        option.clone(),
        ResolvedType::I32,
        false,
        CheckedCoercion::default(),
    );
    let (call, mut resolver) = call_returning(
        CheckedBlock {
            stmts: vec![CheckedStmt::Expression(expr)],
            tail: None,
        },
        option,
    );
    let value = eval(&mut resolver, &call, Target::DEFAULT, None).unwrap();
    assert_eq!(value, fallible_const(1, vec![]));
}

#[test]
fn a_failing_result_try_returns_the_enclosing_err() {
    let result = fallible_enum(
        2,
        "Result",
        "Ok",
        "Err",
        ResolvedType::I32,
        Some(ResolvedType::I32),
    );
    let expr = try_node(
        fallible_const(1, vec![ConstValue::Number(NumberValue::Signed(5))]),
        result.clone(),
        result.clone(),
        ResolvedType::I32,
        true,
        CheckedCoercion::default(),
    );
    let (call, mut resolver) = call_returning(
        CheckedBlock {
            stmts: vec![CheckedStmt::Expression(expr)],
            tail: None,
        },
        result,
    );
    let value = eval(&mut resolver, &call, Target::DEFAULT, None).unwrap();
    assert_eq!(
        value,
        fallible_const(1, vec![ConstValue::Number(NumberValue::Signed(5))])
    );
}

#[test]
fn a_propagated_error_runs_its_recorded_coercion() {
    let result = fallible_enum(
        2,
        "Result",
        "Ok",
        "Err",
        ResolvedType::I32,
        Some(ResolvedType::I32),
    );
    let coercion = CheckedCoercion {
        steps: vec![CheckedCoercionStep::InjectAnonymousMember {
            variant_index: 1,
            target_type: ResolvedType::I32,
        }],
    };
    let expr = try_node(
        fallible_const(1, vec![ConstValue::Number(NumberValue::Signed(5))]),
        result.clone(),
        result.clone(),
        ResolvedType::I32,
        true,
        coercion,
    );
    let (call, mut resolver) = call_returning(
        CheckedBlock {
            stmts: vec![CheckedStmt::Expression(expr)],
            tail: None,
        },
        result,
    );
    let value = eval(&mut resolver, &call, Target::DEFAULT, None).unwrap();
    assert_eq!(
        value,
        fallible_const(
            1,
            vec![ConstValue::anonymous_enum(
                1,
                vec![ConstValue::Number(NumberValue::Signed(5))]
            )]
        )
    );
}

#[test]
fn a_defer_registered_before_a_failing_try_still_runs() {
    let option = fallible_enum(1, "Option", "Some", "None", ResolvedType::I32, None);
    // The deferred body cannot be evaluated at compile time, so the failure
    // it reports is the proof that a `?`-generated return still ran it.
    let defer = CheckedStmt::Defer(crate::checked::CheckedDefer {
        id: id(43),
        span: sp(),
        body: CheckedBlock {
            stmts: vec![CheckedStmt::Expression(node(
                CheckedExpr::BinaryOp(CheckedBinaryOp {
                    op: BinaryOp::Div,
                    left: Box::new(num(1)),
                    right: Box::new(num(0)),
                }),
                ResolvedType::I32,
            ))],
            tail: None,
        },
    });
    let expr = try_node(
        fallible_const(1, vec![]),
        option.clone(),
        option.clone(),
        ResolvedType::I32,
        false,
        CheckedCoercion::default(),
    );
    let (call, mut resolver) = call_returning(
        CheckedBlock {
            stmts: vec![defer, CheckedStmt::Expression(expr)],
            tail: None,
        },
        option,
    );
    let err = eval(&mut resolver, &call, Target::DEFAULT, None).unwrap_err();
    assert!(matches!(err.kind, CompErrorKind::Unsupported(_)));
}

#[test]
fn a_try_that_escapes_the_outermost_evaluation_is_a_diagnostic() {
    let option = fallible_enum(1, "Option", "Some", "None", ResolvedType::I32, None);
    let expr = try_node(
        fallible_const(1, vec![]),
        option.clone(),
        option,
        ResolvedType::I32,
        false,
        CheckedCoercion::default(),
    );
    let err = eval(&mut NoFunctions, &expr, Target::DEFAULT, None).unwrap_err();
    assert!(matches!(err.kind, CompErrorKind::EscapingControlFlow));
}
