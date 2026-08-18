#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Namespace {
    Type,
    Value,
}

impl Namespace {
    pub(crate) fn tag(self) -> char {
        match self {
            Namespace::Type => 't',
            Namespace::Value => 'v',
        }
    }

    pub(crate) fn from_tag(c: char) -> Option<Self> {
        match c {
            't' => Some(Namespace::Type),
            'v' => Some(Namespace::Value),
            _ => None,
        }
    }
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
    SpecObject(Box<MangleType>, bool),
    Function(Vec<MangleType>, Box<MangleType>, bool),
    Named(ManglePath, Option<u32>),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Symbol {
    pub path: ManglePath,
    pub signature: Option<(Vec<MangleType>, MangleType)>,
    pub vendor_suffix: Option<String>,
}
