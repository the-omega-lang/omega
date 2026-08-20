use super::function::FunctionLowerer;
use crate::body::{MirExprNode, MirPlace, MirPlaceRoot, MirProjection};
use omega_analyzer::checked::{
    CheckedExprNode, CheckedPlace, CheckedPlaceRoot, CheckedProjection, Storage,
};
use omega_analyzer::layout;
use omega_analyzer::resolved_type::ResolvedType;

pub(super) fn place_align(r#type: &ResolvedType) -> u32 {
    layout::type_alignment(r#type)
}

pub(super) fn lower_place(lowerer: &mut FunctionLowerer, place: CheckedPlace) -> MirPlace {
    lower_place_with(lowerer, place, |lowerer, e| lowerer.lower_expr(e))
}

/// Lowers `place`, materializing any dynamic component (a computed root
/// expression, or an index expression) into a MIR local exactly once. The
/// resulting `MirPlace` only ever re-reads already-computed locals, so it
/// is safe to lower once here and then clone/reuse for both a load and a
/// store, without re-executing any side-effecting subexpression. Used by
/// compound-assign/increment-decrement lowering, which reads and writes
/// the same place; ordinary single-use places should keep using
/// `lower_place` so they don't pay for locals they don't need.
pub(super) fn lower_place_evaluated_once(
    lowerer: &mut FunctionLowerer,
    place: CheckedPlace,
) -> MirPlace {
    lower_place_with(lowerer, place, |lowerer, e| {
        let lowered = lowerer.lower_expr(e);
        lowerer.materialize_once(lowered)
    })
}

fn lower_place_with(
    lowerer: &mut FunctionLowerer,
    place: CheckedPlace,
    mut lower_dynamic: impl FnMut(&mut FunctionLowerer, CheckedExprNode) -> MirExprNode,
) -> MirPlace {
    let root = match place.root {
        CheckedPlaceRoot::Variable {
            decl_id,
            storage,
            r#type,
        } => match storage {
            // Parameters and locals share the same MIR LocalId namespace.
            Storage::Local | Storage::Parameter => {
                let id = lowerer.local_for_hir(decl_id);
                MirPlaceRoot::Local { id, r#type }
            }
            Storage::Function => MirPlaceRoot::Function(decl_id),
            Storage::Global => MirPlaceRoot::Global {
                id: decl_id,
                r#type,
            },
            Storage::Comp => {
                unreachable!(
                    "analysis substitutes comp bindings into CheckedExpr::Const; see Storage::Comp"
                )
            }
        },
        CheckedPlaceRoot::Expr(e) => MirPlaceRoot::Expr(Box::new(lower_dynamic(lowerer, *e))),
    };
    let projections = place
        .projections
        .into_iter()
        .map(|p| lower_projection_with(lowerer, p, &mut lower_dynamic))
        .collect();
    let align = place_align(&place.r#type);
    MirPlace {
        root,
        projections,
        r#type: place.r#type,
        align,
    }
}

fn lower_projection_with(
    lowerer: &mut FunctionLowerer,
    projection: CheckedProjection,
    lower_dynamic: &mut impl FnMut(&mut FunctionLowerer, CheckedExprNode) -> MirExprNode,
) -> MirProjection {
    match projection {
        CheckedProjection::FieldAccess {
            field,
            index,
            r#type,
        } => MirProjection::FieldAccess {
            field,
            index,
            r#type,
        },
        CheckedProjection::Index {
            index_expr,
            item_type,
        } => MirProjection::Index {
            index_expr: Box::new(lower_dynamic(lowerer, *index_expr)),
            item_type,
        },
        CheckedProjection::Deref { r#type } => MirProjection::Deref { r#type },
        CheckedProjection::SliceLength => MirProjection::SliceLength,
        CheckedProjection::SpecObjectPtr { mutable } => MirProjection::SpecObjectPtr { mutable },
        CheckedProjection::SpecObjectVtable => MirProjection::SpecObjectVtable,
        CheckedProjection::EnumTag { r#type } => MirProjection::EnumTag { r#type },
        CheckedProjection::EnumHeader {
            field,
            index,
            r#type,
        } => MirProjection::EnumHeader {
            field,
            index,
            r#type,
        },
        CheckedProjection::EnumDynamicField {
            field,
            index,
            r#type,
        } => MirProjection::EnumDynamicField {
            field,
            index,
            r#type,
        },
        CheckedProjection::EnumBody {
            variant_index,
            field_index,
            r#type,
        } => MirProjection::EnumBody {
            variant_index,
            field_index,
            r#type,
        },
        CheckedProjection::UnionField {
            field,
            index,
            r#type,
        } => MirProjection::UnionField {
            field,
            index,
            r#type,
        },
    }
}
