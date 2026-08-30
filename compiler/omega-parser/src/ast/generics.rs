use crate::ast::identifier::Ident;
use crate::ast::r#type::{GenericArg, Type};

/// What one generic parameter binds. A type parameter binds a type and may
/// carry spec bounds; a `comp` parameter binds a compile-time value of a
/// mandatory declared type and never carries bounds.
#[derive(Debug, Clone)]
pub enum GenericParamKind {
    Type { bounds: Vec<Type> },
    Comp { value_type: Type },
}

#[derive(Debug, Clone)]
pub struct GenericParam {
    pub ident: Ident,
    pub kind: GenericParamKind,
    pub default: Option<GenericArg>,
}

impl GenericParam {
    pub fn r#type(ident: Ident, bounds: Vec<Type>, default: Option<GenericArg>) -> Self {
        Self {
            ident,
            kind: GenericParamKind::Type { bounds },
            default,
        }
    }

    pub fn bounds(&self) -> &[Type] {
        match &self.kind {
            GenericParamKind::Type { bounds } => bounds,
            GenericParamKind::Comp { .. } => &[],
        }
    }

    pub fn comp_type(&self) -> Option<&Type> {
        match &self.kind {
            GenericParamKind::Comp { value_type } => Some(value_type),
            GenericParamKind::Type { .. } => None,
        }
    }

    pub fn is_comp(&self) -> bool {
        matches!(self.kind, GenericParamKind::Comp { .. })
    }
}
