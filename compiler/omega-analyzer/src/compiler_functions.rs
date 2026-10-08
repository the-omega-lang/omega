//! Declarations whose bodies the compiler supplies. The rules live in
//! `docs/language/volatile.md` and `docs/language/reflection.md`; this module
//! is their single owner.

use crate::error::AnalysisErrorKind;
use omega_hir::HirFunctionDef;
use omega_parser::prelude::{GenericParamKind, Ident, Path, Type, Visibility};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompilerFunction {
    ReadVolatile,
    WriteVolatile,
    TypeInfo,
}

impl CompilerFunction {
    const ALL: [Self; 3] = [Self::ReadVolatile, Self::WriteVolatile, Self::TypeInfo];

    pub fn module(self) -> [&'static str; 2] {
        match self {
            Self::ReadVolatile | Self::WriteVolatile => ["core", "volatile"],
            Self::TypeInfo => ["core", "reflection"],
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::ReadVolatile => "read_volatile",
            Self::WriteVolatile => "write_volatile",
            Self::TypeInfo => "typeinfo",
        }
    }

    fn from_path(module_path: &[Ident], name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|function| {
            function.name() == name && module_path.iter().map(Ident::as_ref).eq(function.module())
        })
    }

    fn matches_declaration(self, f: &HirFunctionDef) -> bool {
        let [generic] = f.generics.as_slice() else {
            return false;
        };
        let GenericParamKind::Type { bounds } = &generic.kind else {
            return false;
        };
        if !bounds.is_empty() || generic.default.is_some() {
            return false;
        }
        if f.body.is_some()
            || f.visibility != Visibility::Exposed
            || !f.annotations.is_empty()
            || f.self_mode.is_some()
        {
            return false;
        }

        let named = |name: &str| Type::Named(Path::from(Ident(name.to_string())));
        let t = Type::Named(Path::from(generic.ident.clone()));
        let pointer = |mutable| Type::Pointer(Box::new(t.clone()), mutable);
        let (expected_params, expected_return) = match self {
            Self::ReadVolatile => (vec![("location", pointer(false))], t.clone()),
            Self::WriteVolatile => (
                vec![("location", pointer(true)), ("value", t.clone())],
                named("void"),
            ),
            Self::TypeInfo => (
                Vec::new(),
                Type::Pointer(Box::new(named("TypeInfo")), false),
            ),
        };
        f.return_type == expected_return
            && f.params.len() == expected_params.len()
            && f.params
                .iter()
                .zip(&expected_params)
                .all(|(param, (name, ty))| param.ident.as_ref() == *name && param.r#type == *ty)
    }
}

/// Classifies a free function declared in `module_path`. A recognized path
/// with the wrong declaration shape is an error even when it has a body, so a
/// compiler-implemented name never silently becomes an ordinary function.
pub fn compiler_function(
    module_path: &[Ident],
    f: &HirFunctionDef,
) -> Result<Option<CompilerFunction>, AnalysisErrorKind> {
    match CompilerFunction::from_path(module_path, f.name.as_ref()) {
        Some(function) if function.matches_declaration(f) => Ok(Some(function)),
        Some(function) => Err(AnalysisErrorKind::MalformedCompilerFunction {
            module: function.module().join("::"),
            name: f.name.clone(),
        }),
        None if f.body.is_none() => Err(AnalysisErrorKind::FunctionWithoutBody {
            name: f.name.clone(),
        }),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests;
