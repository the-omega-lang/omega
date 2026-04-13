use omega_parser::prelude::{FunctionType, Ident, Type};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedFunctionType {
    pub params: Vec<(Ident, ResolvedType)>,
    pub return_type: Box<ResolvedType>,
    pub is_variadic: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedType {
    Void,
    Char,
    I32,
    Pointer(Box<ResolvedType>),
    Function(ResolvedFunctionType),
}

impl TryFrom<FunctionType> for ResolvedFunctionType {
    type Error = String;
    fn try_from(value: FunctionType) -> Result<Self, Self::Error> {
        Ok(ResolvedFunctionType {
            params: value
                .params
                .into_iter()
                .map(|(ident, typ)| ResolvedType::try_from(typ).map(|resolved| (ident, resolved)))
                .collect::<Result<Vec<(Ident, ResolvedType)>, Self::Error>>()?,
            return_type: Box::new(ResolvedType::try_from(*value.return_type)?),
            is_variadic: value.is_variadic,
        })
    }
}

impl TryFrom<Type> for ResolvedType {
    type Error = String;
    fn try_from(value: Type) -> Result<Self, Self::Error> {
        let resolved = match value {
            Type::Named(Ident(name)) => match name.as_str() {
                "void" => ResolvedType::Void,
                "i32" => ResolvedType::I32,
                "char" => ResolvedType::Char,
                _ => return Err(format!("Unrecognized named type: {}", name)),
            },
            Type::Pointer(typ) => ResolvedType::Pointer(Box::new(Self::try_from(*typ.to_owned())?)),
            Type::Function(fntyp) => ResolvedType::Function(ResolvedFunctionType::try_from(fntyp)?),
        };

        Ok(resolved)
    }
}
