use super::*;
use crate::checked::{CheckedTry, CheckedTryDestination, CheckedTryKind, CheckedTrySource};
use omega_hir::HirTry;

/// The variant and payload names a canonical fallible enum is required to
/// declare. Resolving them by name here is what keeps `?` off `Option`'s
/// accidental `None`/`Some` variant order.
struct FallibleNames {
    success: &'static str,
    failure: &'static str,
    success_payload: &'static str,
    failure_payload: Option<&'static str>,
}

/// Everything `?` needs to know about one side of the propagation, read out
/// of an enum declaration exactly once.
struct FallibleFacts {
    tag_type: ResolvedType,
    success_variant: usize,
    success_tag: NumberValue,
    success_field: usize,
    success_type: ResolvedType,
    failure_variant: usize,
    /// The failure payload's field index and type. `None` for
    /// `Option::None`, which carries no payload.
    failure_payload: Option<(usize, ResolvedType)>,
}

impl CheckedTryKind {
    fn names(self) -> FallibleNames {
        match self {
            Self::Option => FallibleNames {
                success: "Some",
                failure: "None",
                success_payload: "value",
                failure_payload: None,
            },
            Self::Result => FallibleNames {
                success: "Ok",
                failure: "Err",
                success_payload: "value",
                failure_payload: Some("error"),
            },
        }
    }
}

impl<'r> Analyzer<'r> {
    pub(super) fn analyze_try(
        &mut self,
        id: HirId,
        span: Span,
        r#try: &HirTry,
    ) -> Option<CheckedExprNode> {
        let operator_span = r#try.operator_span;
        if self.in_defer_body {
            self.error(id, operator_span, AnalysisErrorKind::TryInsideDefer);
            return None;
        }

        // The operand carries its own fallible type; the surrounding expected
        // type describes the unwrapped success value and must not be pushed
        // into it.
        let operand = self.analyze_expr(&r#try.base, None)?;

        let Some((kind, source_cell)) = canonical_fallible(&operand.r#type) else {
            self.error(
                id,
                operator_span,
                AnalysisErrorKind::TryOperandNotFallible {
                    found: operand.r#type.clone(),
                },
            );
            return None;
        };

        let return_type = self.current_return_type.clone();
        let Some((return_kind, destination_cell)) = canonical_fallible(&return_type) else {
            self.error(
                id,
                operator_span,
                AnalysisErrorKind::TryOutsideFallibleFunction {
                    operand: kind.type_name(),
                    r#return: return_type,
                },
            );
            return None;
        };
        if return_kind != kind {
            self.error(
                id,
                operator_span,
                AnalysisErrorKind::TryFamilyMismatch {
                    operand: kind.type_name(),
                    r#return: return_type,
                    returned: return_kind.type_name(),
                },
            );
            return None;
        }

        let names = kind.names();
        let (Some(source), Some(destination)) = (
            fallible_facts(&source_cell, &names),
            fallible_facts(&destination_cell, &names),
        ) else {
            self.error(
                id,
                operator_span,
                AnalysisErrorKind::TryOperandNotFallible {
                    found: operand.r#type.clone(),
                },
            );
            return None;
        };

        let error_coercion = match (&source.failure_payload, &destination.failure_payload) {
            (Some((_, found)), Some((_, expected))) => {
                let Some(plan) = self.plan_coercion(id, operator_span, expected, found) else {
                    self.error(
                        id,
                        operator_span,
                        AnalysisErrorKind::TryErrorNotPropagatable {
                            found: found.clone(),
                            expected: expected.clone(),
                        },
                    );
                    return None;
                };
                plan
            }
            _ => CheckedCoercion::default(),
        };

        Some(CheckedExprNode {
            id,
            span,
            r#type: source.success_type,
            kind: CheckedExpr::Try(CheckedTry {
                operand: Box::new(operand),
                operator_span,
                kind,
                source: CheckedTrySource {
                    tag_type: source.tag_type,
                    success_variant: source.success_variant,
                    success_tag: source.success_tag,
                    success_field: source.success_field,
                    failure_variant: source.failure_variant,
                    failure_payload: source.failure_payload,
                },
                destination: CheckedTryDestination {
                    r#type: return_type,
                    failure_variant: destination.failure_variant,
                    failure_field: destination.failure_payload.map(|(field, _)| field),
                    error_coercion,
                },
            }),
        })
    }
}

/// Recognizes `core::option::Option` and `core::result::Result` by resolved
/// declaration identity rather than by spelling, so a transparent alias keeps
/// try behavior and a user-defined lookalike never gains it. Refinement is
/// ignored: a value already proven to be one variant is still a value of the
/// enum.
fn canonical_fallible(
    r#type: &ResolvedType,
) -> Option<(CheckedTryKind, Rc<RefCell<ResolvedEnumType>>)> {
    let ResolvedType::Enum { cell, .. } = r#type else {
        return None;
    };
    let kind = {
        let definition = cell.borrow();
        let module: Vec<&str> = definition.module_path.iter().map(Ident::as_ref).collect();
        match (module.as_slice(), definition.name.as_ref()) {
            (["core", "option"], "Option") => CheckedTryKind::Option,
            (["core", "result"], "Result") => CheckedTryKind::Result,
            _ => return None,
        }
    };
    Some((kind, cell.clone()))
}

fn fallible_facts(
    cell: &Rc<RefCell<ResolvedEnumType>>,
    names: &FallibleNames,
) -> Option<FallibleFacts> {
    let definition = cell.borrow();
    let (success_variant, success) = definition.variant(&Ident(names.success.to_string()))?;
    let (failure_variant, failure) = definition.variant(&Ident(names.failure.to_string()))?;
    let (success_field, success_type) = field_of(success, names.success_payload)?;
    let failure_payload = match names.failure_payload {
        Some(name) => Some(field_of(failure, name)?),
        None => None,
    };
    Some(FallibleFacts {
        tag_type: definition.tag_type.clone(),
        success_variant,
        success_tag: success.tag,
        success_field,
        success_type,
        failure_variant,
        failure_payload,
    })
}

fn field_of(variant: &ResolvedEnumVariant, name: &str) -> Option<(usize, ResolvedType)> {
    variant
        .fields
        .iter()
        .position(|field| field.name.as_ref() == name)
        .map(|index| (index, variant.fields[index].r#type.clone()))
}
