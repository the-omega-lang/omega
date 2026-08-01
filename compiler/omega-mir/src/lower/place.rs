//! `CheckedPlace -> MirPlace` -- resolves a `Storage::Local`/
//! `Storage::Parameter` reference to the `LocalId` `FunctionLowerer`
//! already allocated for it (the lowering-time counterpart of
//! `Codegen::resolve_place_storage`'s identical lookup, just against
//! `local_of` instead of `stack_slots`/`local_args`).

use super::function::FunctionLowerer;
use crate::body::{MirPlace, MirPlaceRoot, MirProjection};
use omega_analyzer::checked::{CheckedPlace, CheckedPlaceRoot, CheckedProjection, Storage};

pub(super) fn lower_place(lowerer: &mut FunctionLowerer, place: CheckedPlace) -> MirPlace {
    let root = match place.root {
        CheckedPlaceRoot::Variable { decl_id, storage, r#type } => match storage {
            // Parameters and declared locals both just index into
            // `MirBody::locals` uniformly -- see its own doc comment for
            // why that unification is sound.
            Storage::Local | Storage::Parameter => {
                let id = *lowerer.local_of.get(&decl_id).unwrap_or_else(|| {
                    panic!("checked module guarantees {decl_id:?} was declared before this use")
                });
                MirPlaceRoot::Local { id, r#type }
            }
            Storage::Function => MirPlaceRoot::Function(decl_id),
            Storage::Global => MirPlaceRoot::Global { id: decl_id, r#type },
            Storage::Comp => {
                unreachable!("a comp binding is substituted into CheckedExpr::Const during analysis -- see Storage::Comp's doc comment")
            }
        },
        CheckedPlaceRoot::Expr(e) => MirPlaceRoot::Expr(Box::new(lowerer.lower_expr(*e))),
    };
    let projections = place.projections.into_iter().map(|p| lower_projection(lowerer, p)).collect();
    MirPlace { root, projections }
}

fn lower_projection(lowerer: &mut FunctionLowerer, projection: CheckedProjection) -> MirProjection {
    match projection {
        CheckedProjection::FieldAccess { field, index, r#type } => MirProjection::FieldAccess { field, index, r#type },
        CheckedProjection::Index { index_expr, item_type } => {
            MirProjection::Index { index_expr: Box::new(lowerer.lower_expr(*index_expr)), item_type }
        }
        CheckedProjection::Deref { r#type } => MirProjection::Deref { r#type },
        CheckedProjection::SliceLength => MirProjection::SliceLength,
        CheckedProjection::SpecObjectPtr { mutable } => MirProjection::SpecObjectPtr { mutable },
        CheckedProjection::SpecObjectVtable => MirProjection::SpecObjectVtable,
        CheckedProjection::EnumTag { r#type } => MirProjection::EnumTag { r#type },
        CheckedProjection::EnumHeader { field, index, r#type } => {
            MirProjection::EnumHeader { field, index, r#type }
        }
        CheckedProjection::EnumDynamicField { field, index, r#type } => {
            MirProjection::EnumDynamicField { field, index, r#type }
        }
        CheckedProjection::EnumBody { variant_index, field_index, r#type } => {
            MirProjection::EnumBody { variant_index, field_index, r#type }
        }
        CheckedProjection::UnionField { field, index, r#type } => MirProjection::UnionField { field, index, r#type },
    }
}
