use crate::ast::identifier::Ident;
use crate::ast::r#type::{GenericArg, Type};
use crate::diagnostics::Span;

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

/// One generic argument written on a *function* path in expression position.
///
/// Beyond an ordinary argument, a caller may name the exact bound set a
/// declaration's parameter must declare, which is how overlapping generic
/// overloads are chosen between. `M: A + B` fixes the slot and constrains
/// the declaration; `spec A + B` only constrains it, leaving the slot to
/// ordinary inference.
#[derive(Debug, Clone)]
pub enum ExprGenericArg {
    Plain(GenericArg),
    Bounded {
        arg: GenericArg,
        bounds: Vec<Type>,
        span: Span,
    },
    Inferred {
        bounds: Vec<Type>,
        span: Span,
    },
}

// Spans are provenance, not syntax, and expression paths compare
// structurally; a selector's identity is the argument and the bounds.
impl PartialEq for ExprGenericArg {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Plain(left), Self::Plain(right)) => left == right,
            (
                Self::Bounded {
                    arg: left,
                    bounds: left_bounds,
                    ..
                },
                Self::Bounded {
                    arg: right,
                    bounds: right_bounds,
                    ..
                },
            ) => left == right && left_bounds == right_bounds,
            (Self::Inferred { bounds: left, .. }, Self::Inferred { bounds: right, .. }) => {
                left == right
            }
            _ => false,
        }
    }
}

impl Eq for ExprGenericArg {}

impl ExprGenericArg {
    /// The ordinary argument this entry is, when it carries no selector.
    /// Positions that do not accept a selector read a written list through
    /// this and reject whatever it leaves out.
    pub fn plain(&self) -> Option<&GenericArg> {
        match self {
            Self::Plain(arg) => Some(arg),
            _ => None,
        }
    }

    /// What this entry binds, whether or not it also selects.
    pub fn arg(&self) -> Option<&GenericArg> {
        match self {
            Self::Plain(arg) | Self::Bounded { arg, .. } => Some(arg),
            Self::Inferred { .. } => None,
        }
    }

    pub fn selector(&self) -> Option<&[Type]> {
        match self {
            Self::Plain(_) => None,
            Self::Bounded { bounds, .. } | Self::Inferred { bounds, .. } => Some(bounds),
        }
    }

    pub fn selector_span(&self) -> Option<Span> {
        match self {
            Self::Plain(_) => None,
            Self::Bounded { span, .. } | Self::Inferred { span, .. } => Some(*span),
        }
    }
}

impl From<GenericArg> for ExprGenericArg {
    fn from(arg: GenericArg) -> Self {
        Self::Plain(arg)
    }
}
