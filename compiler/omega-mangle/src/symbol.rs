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
    Generic(Box<ManglePath>, Vec<MangleType>),
    Type(Box<MangleType>),
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
