#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Namespace {
    Type,
    Value,
}

/// A function type/signature's calling convention, mirrored from
/// `omega_analyzer::resolved_type::CallingConvention` so this standalone
/// crate stays dependency-free. `Omega` is the implicit default and is never
/// written to the wire; every other convention gets its own marker so
/// existing ordinary-function encodings are unaffected byte-for-byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum MangleConvention {
    #[default]
    Omega,
    C,
    SysV64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ManglePath {
    Root(String),
    Nested(Box<ManglePath>, Namespace, String),
    /// An all-type generic application. Kept as its own form so every
    /// pre-existing generic symbol keeps its exact bytes.
    Generic(Box<ManglePath>, Vec<MangleType>),
    /// A generic application with at least one compile-time value argument.
    /// Each element carries an explicit argument tag, so a value can never be
    /// confused with a type.
    MixedGeneric(Box<ManglePath>, Vec<MangleGenericArg>),
    /// A generic function declaration's structural identity, applied to the
    /// path that names it and encoded *before* its concrete arguments. See
    /// [`MangleTemplate`].
    Template(Box<ManglePath>, Box<MangleTemplate>),
    Type(Box<MangleType>),
}

impl ManglePath {
    /// The narrowest form that can represent `args`: an all-type list keeps
    /// the legacy encoding, and only a value argument switches to the mixed
    /// one.
    pub fn generic(parent: ManglePath, args: Vec<MangleGenericArg>) -> Self {
        let types: Option<Vec<MangleType>> = args
            .iter()
            .map(|arg| match arg {
                MangleGenericArg::Type(ty) => Some(ty.clone()),
                MangleGenericArg::Value(_) => None,
            })
            .collect();
        match types {
            Some(types) => Self::Generic(Box::new(parent), types),
            None => Self::MixedGeneric(Box::new(parent), args),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MangleGenericArg {
    Type(MangleType),
    Value(MangleValue),
}

/// A compile-time generic argument value. Its type is part of its identity,
/// so the same digits under two different `comp` parameter types produce two
/// different symbols.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MangleValue {
    Int { r#type: MangleIntType, value: i128 },
    Bool(bool),
    Char(char),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MangleIntType {
    I8,
    I16,
    I32,
    I64,
    ISize,
    U8,
    U16,
    U32,
    U64,
    USize,
}

impl MangleIntType {
    pub fn mangle_type(self) -> MangleType {
        match self {
            Self::I8 => MangleType::I8,
            Self::I16 => MangleType::I16,
            Self::I32 => MangleType::I32,
            Self::I64 => MangleType::I64,
            Self::ISize => MangleType::ISize,
            Self::U8 => MangleType::U8,
            Self::U16 => MangleType::U16,
            Self::U32 => MangleType::U32,
            Self::U64 => MangleType::U64,
            Self::USize => MangleType::USize,
        }
    }

    pub fn from_mangle_type(ty: &MangleType) -> Option<Self> {
        Some(match ty {
            MangleType::I8 => Self::I8,
            MangleType::I16 => Self::I16,
            MangleType::I32 => Self::I32,
            MangleType::I64 => Self::I64,
            MangleType::ISize => Self::ISize,
            MangleType::U8 => Self::U8,
            MangleType::U16 => Self::U16,
            MangleType::U32 => Self::U32,
            MangleType::U64 => Self::U64,
            MangleType::USize => Self::USize,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MangleType {
    Void,
    Never,
    Bool,
    Char,
    I8,
    I16,
    I32,
    I64,
    ISize,
    U8,
    U16,
    U32,
    U64,
    USize,
    F32,
    F64,
    Pointer(Box<MangleType>, bool),
    Slice(Box<MangleType>, bool),
    Str(bool),
    Array(Box<MangleType>, bool),
    SizedArray(Box<MangleType>, u64),
    /// One dynamic spec-object shape, member types in canonical final-name
    /// order. A single-member shape mangles byte-identically to the
    /// pre-conjunction singleton encoding.
    SpecObject(Vec<MangleType>, bool),
    /// One anonymous enum, members in the analyzer's canonical order. The
    /// optional index is a member refinement, mirroring `Named`'s variant
    /// refinement. Nothing downstream may reorder the members: canonical
    /// order is what makes `enum A | B` and `enum B | A` one symbol.
    AnonymousEnum(Vec<MangleType>, Option<u32>),
    Function(Vec<MangleType>, Box<MangleType>, bool, MangleConvention),
    Named(ManglePath, Option<u32>),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FunctionSignature {
    pub params: Vec<MangleType>,
    pub return_type: MangleType,
    pub is_variadic: bool,
    pub convention: MangleConvention,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Symbol {
    pub path: ManglePath,
    pub signature: Option<FunctionSignature>,
    pub vendor_suffix: Option<String>,
}

/// The structural identity of a generic function *declaration*, before its
/// own generic parameters are substituted.
///
/// Two declarations of one name reachable at the same concrete arguments --
/// which explicit bound selection makes possible -- differ here, so they
/// stay two linker symbols instead of folding onto one weak definition.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MangleTemplate {
    /// The declaration's own generic parameters, in declared order.
    pub generics: Vec<MangleTemplateParam>,
    pub params: Vec<MangleTemplateType>,
    pub return_type: MangleTemplateType,
    pub is_variadic: bool,
    pub convention: MangleConvention,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MangleTemplateParam {
    /// A type parameter and the exact bound set it declares, in the
    /// producer's canonical order.
    Type(Vec<MangleTemplateBound>),
    /// A `comp` parameter and its declared value type.
    Comp(MangleTemplateType),
}

impl MangleTemplateParam {
    fn is_comp(&self) -> bool {
        matches!(self, Self::Comp(_))
    }
}

/// One spec application: its full declared package/module path and the
/// symbolic arguments applied to it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MangleTemplateBound {
    pub spec: ManglePath,
    pub args: Vec<MangleTemplateArg>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MangleTemplateArg {
    Type(MangleTemplateType),
    Value(MangleValue),
    /// A reference to the declaration's own `comp` parameter at this index.
    Param(u32),
}

/// A type as the declaration writes it: concrete leaves stay concrete, and
/// every mention of the declaration's own parameters is a position, so two
/// declarations whose parameters differ only in name are one identity while
/// `<M: A>` and a bound fixed to `i32` stay distinct.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MangleTemplateType {
    /// A reference to the declaration's own type parameter at this index.
    Param(u32),
    Fixed(MangleType),
    Pointer(Box<Self>, bool),
    Slice(Box<Self>, bool),
    Array(Box<Self>, bool),
    SizedArray(Box<Self>, Box<MangleTemplateArg>),
    SpecObject(Vec<MangleTemplateBound>, bool),
    AnonymousEnum(Vec<Self>),
    Function(Vec<Self>, Box<Self>, bool, MangleConvention),
    Nominal(ManglePath, Vec<MangleTemplateArg>),
}

impl MangleTemplate {
    /// Whether every symbolic reference in this descriptor names one of its
    /// own parameters, with the kind that parameter declares. A decoder
    /// rejects a descriptor that fails this; an encoder never builds one.
    pub fn references_are_valid(&self) -> bool {
        self.generics.iter().all(|generic| match generic {
            MangleTemplateParam::Type(bounds) => {
                bounds.iter().all(|bound| self.bound_is_valid(bound))
            }
            MangleTemplateParam::Comp(value_type) => self.type_is_valid(value_type),
        }) && self.params.iter().all(|param| self.type_is_valid(param))
            && self.type_is_valid(&self.return_type)
    }

    fn bound_is_valid(&self, bound: &MangleTemplateBound) -> bool {
        bound.args.iter().all(|arg| self.arg_is_valid(arg))
    }

    fn arg_is_valid(&self, arg: &MangleTemplateArg) -> bool {
        match arg {
            MangleTemplateArg::Type(ty) => self.type_is_valid(ty),
            MangleTemplateArg::Value(_) => true,
            MangleTemplateArg::Param(index) => self
                .generics
                .get(*index as usize)
                .is_some_and(MangleTemplateParam::is_comp),
        }
    }

    fn type_is_valid(&self, ty: &MangleTemplateType) -> bool {
        match ty {
            MangleTemplateType::Param(index) => self
                .generics
                .get(*index as usize)
                .is_some_and(|generic| !generic.is_comp()),
            MangleTemplateType::Fixed(_) => true,
            MangleTemplateType::Pointer(inner, _)
            | MangleTemplateType::Slice(inner, _)
            | MangleTemplateType::Array(inner, _) => self.type_is_valid(inner),
            MangleTemplateType::SizedArray(inner, length) => {
                self.type_is_valid(inner) && self.arg_is_valid(length)
            }
            MangleTemplateType::SpecObject(members, _) => {
                members.iter().all(|member| self.bound_is_valid(member))
            }
            MangleTemplateType::AnonymousEnum(members) => {
                members.iter().all(|member| self.type_is_valid(member))
            }
            MangleTemplateType::Function(params, return_type, _, _) => {
                params.iter().all(|param| self.type_is_valid(param))
                    && self.type_is_valid(return_type)
            }
            MangleTemplateType::Nominal(_, args) => args.iter().all(|arg| self.arg_is_valid(arg)),
        }
    }
}
