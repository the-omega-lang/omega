use crate::generics::pattern::TypePattern;
use crate::resolved_type::ResolvedType;

/// What the context of an expression says about its type.
///
/// A binding annotation with `_` holes is a `Pattern`: its known parts are
/// fixed and every `TypePattern::Parameter` in it is a hole, never a callee's
/// generic parameter. Only the sites that choose something from the expected
/// type read a pattern; everything else sees it through [`Self::exact`] as no
/// expectation at all.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) enum Expected<'a> {
    #[default]
    None,
    Exact(&'a ResolvedType),
    Pattern(&'a TypePattern),
}

impl<'a> Expected<'a> {
    pub(crate) fn exact(self) -> Option<&'a ResolvedType> {
        match self {
            Self::Exact(r#type) => Some(r#type),
            Self::None | Self::Pattern(_) => None,
        }
    }

    /// The expectation one component of a pattern places on the matching
    /// component of an expression.
    pub(crate) fn narrow(pattern: &'a TypePattern) -> Self {
        match pattern {
            TypePattern::Fixed(r#type) => Self::Exact(r#type),
            TypePattern::Parameter(_) => Self::None,
            _ => Self::Pattern(pattern),
        }
    }
}

impl<'a> From<Option<&'a ResolvedType>> for Expected<'a> {
    fn from(expected: Option<&'a ResolvedType>) -> Self {
        expected.map_or(Self::None, Self::Exact)
    }
}
