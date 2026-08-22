use omega_hir::{HirBlock, HirId, HirParam};
use omega_parser::prelude::{Ident, SelfMode, Span, Type, Visibility};
use std::cell::RefCell;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

/// The resolved calling convention of a function type. `Omega` is the
/// implicit convention of every ordinary Omega function type; the others are
/// selected only by explicit `foreign(cc)` syntax. Distinct even where a
/// target happens to lower two conventions identically -- see
/// `docs/language/foreign-function-interface.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CallingConvention {
    Omega,
    C,
    SysV64,
}

impl CallingConvention {
    /// Whether this convention can express a variadic tail at all, before
    /// any target gating. Ordinary Omega functions are never variadic.
    pub fn supports_variadic(self) -> bool {
        match self {
            Self::Omega => false,
            Self::C | Self::SysV64 => true,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Omega => "omega",
            Self::C => "c",
            Self::SysV64 => "sysv64",
        }
    }

    /// Whether this convention is meaningful on `target` at all. `sysv64`
    /// names one specific machine convention and must fail semantically off
    /// its supported x86-64 targets rather than silently falling back.
    pub fn is_available_on(self, target: crate::target::Target) -> bool {
        match self {
            Self::Omega | Self::C => true,
            Self::SysV64 => target.arch == crate::target::Arch::X86_64,
        }
    }
}

impl std::fmt::Display for CallingConvention {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ResolvedFunctionType {
    pub params: Vec<(Ident, ResolvedType)>,
    pub return_type: Box<ResolvedType>,
    pub is_variadic: bool,
    pub self_mode: Option<SelfMode>,
    pub calling_convention: CallingConvention,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedMethod {
    pub decl_id: HirId,
    pub fn_type: ResolvedFunctionType,
    pub visibility: Visibility,
    pub annotations: crate::annotations::ResolvedAnnotations,
    pub source: Option<ConformanceSource>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConformanceSource {
    pub spec: Rc<RefCell<ResolvedSpecType>>,
    pub spec_args: Vec<ResolvedType>,
}

#[derive(Debug, Clone)]
pub struct ResolvedConformance {
    pub target: ResolvedType,
    pub spec: Rc<RefCell<ResolvedSpecType>>,
    pub spec_args: Vec<ResolvedType>,
    pub methods: Vec<(Ident, ResolvedMethod)>,
}

#[derive(Debug, Clone)]
pub struct ResolvedBound {
    pub target: ResolvedType,
    pub spec: Rc<RefCell<ResolvedSpecType>>,
    pub spec_args: Vec<ResolvedType>,
}

impl ResolvedBound {
    pub fn new(
        target: ResolvedType,
        spec: Rc<RefCell<ResolvedSpecType>>,
        spec_args: Vec<ResolvedType>,
    ) -> Self {
        Self {
            target,
            spec,
            spec_args,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedField {
    pub name: Ident,
    pub r#type: ResolvedType,
    pub visibility: Visibility,
}

impl ResolvedField {
    pub fn new(name: Ident, r#type: ResolvedType, visibility: Visibility) -> Self {
        Self {
            name,
            r#type,
            visibility,
        }
    }
}

#[derive(Debug)]
pub struct ResolvedStructType {
    pub id: HirId,
    pub name: Ident,
    pub module_path: Vec<Ident>,
    pub type_args: Vec<ResolvedType>,
    pub fields: Vec<ResolvedField>,
    pub functions: Vec<(Ident, ResolvedMethod)>,
    pub layout: crate::annotations::Layout,
    pub suppress: Vec<Ident>,
    pub is_marker: bool,
}

impl PartialEq for ResolvedStructType {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl Eq for ResolvedStructType {}

impl Hash for ResolvedStructType {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

#[derive(Debug)]
pub struct ResolvedUnionType {
    pub id: HirId,
    pub name: Ident,
    pub module_path: Vec<Ident>,
    pub type_args: Vec<ResolvedType>,
    pub fields: Vec<ResolvedField>,
    pub functions: Vec<(Ident, ResolvedMethod)>,
    pub suppress: Vec<Ident>,
}

impl PartialEq for ResolvedUnionType {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl Eq for ResolvedUnionType {}

impl Hash for ResolvedUnionType {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

#[derive(Debug)]
pub struct ResolvedEnumType {
    pub id: HirId,
    pub name: Ident,
    pub module_path: Vec<Ident>,
    pub type_args: Vec<ResolvedType>,
    pub tag_type: ResolvedType,
    pub header: Vec<ResolvedField>,
    pub dynamic_fields: Vec<ResolvedField>,
    pub variants: Vec<ResolvedEnumVariant>,
    pub functions: Vec<(Ident, ResolvedMethod)>,
    pub layout: crate::annotations::Layout,
    pub suppress: Vec<Ident>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedEnumVariant {
    pub name: Ident,
    pub tag: crate::checked::NumberValue,
    pub header_values: Vec<ConstValue>,
    pub fields: Vec<ResolvedField>,
}

impl ResolvedEnumType {
    pub fn variant(&self, name: &Ident) -> Option<(usize, &ResolvedEnumVariant)> {
        self.variants
            .iter()
            .enumerate()
            .find(|(_, v)| &v.name == name)
    }
}

impl PartialEq for ResolvedEnumType {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl Eq for ResolvedEnumType {}

impl Hash for ResolvedEnumType {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

#[derive(Debug)]
pub struct ResolvedSpecType {
    pub id: HirId,
    pub name: Ident,
    pub visibility: Visibility,
    pub generics: Vec<Ident>,
    pub module_path: Vec<Ident>,
    pub type_args: Vec<ResolvedType>,
    pub is_object_safe: bool,
    pub is_alias: bool,
    pub dependencies: Vec<(Rc<RefCell<ResolvedSpecType>>, Vec<Type>)>,
    pub functions: Vec<(Ident, RawSpecFunctionSig)>,
    pub suppress: Vec<Ident>,
}

impl PartialEq for ResolvedSpecType {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl Eq for ResolvedSpecType {}

impl Hash for ResolvedSpecType {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

#[derive(Debug, Clone)]
pub struct RawSpecFunctionSig {
    pub decl_id: HirId,
    pub name: Ident,
    pub span: Span,
    pub name_span: Span,
    pub signature_span: Span,
    pub return_type_span: Span,
    pub visibility: Visibility,
    pub self_mode: Option<SelfMode>,
    pub params: Vec<HirParam>,
    pub is_variadic: bool,
    pub return_type: Type,
    pub default_body: Option<HirBlock>,
}

#[derive(Debug, Clone)]
pub struct GapFunction {
    pub decl_id: HirId,
    pub span: Span,
    pub fn_type: ResolvedFunctionType,
}

#[derive(Debug, Clone)]
pub struct ResolvedGap {
    pub id: HirId,
    pub name: Ident,
    pub module_path: Vec<Ident>,
    pub span: Span,
    pub functions: Vec<(Ident, GapFunction)>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConstValue {
    Number(crate::checked::NumberValue),
    Bool(bool),
    Char(char),
    Str(String),
    Slice(Vec<ConstValue>),
    Array(Vec<ConstValue>),
    Struct(Vec<ConstValue>),
    Enum {
        variant_index: usize,
        tag: crate::checked::NumberValue,
        header: Vec<ConstValue>,
        dynamic_fields: Vec<ConstValue>,
        fields: Vec<ConstValue>,
    },
    Union {
        field_index: usize,
        value: Box<ConstValue>,
    },
    Ref(Box<ConstValue>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumericKind {
    Signed(u32),
    Unsigned(u32),
    Float(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CastClass {
    Int { width: u32, signed: bool },
    Float { width: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedType {
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
    Pointer {
        pointee: Box<ResolvedType>,
        mutable: bool,
    },
    Function(ResolvedFunctionType),
    Array(Box<ResolvedType>, bool),
    SizedArray(Box<ResolvedType>, u32),
    Slice {
        item: Box<ResolvedType>,
        mutable: bool,
    },
    Str {
        mutable: bool,
    },
    Struct(Rc<RefCell<ResolvedStructType>>),
    Union(Rc<RefCell<ResolvedUnionType>>),
    Enum {
        cell: Rc<RefCell<ResolvedEnumType>>,
        variant: Option<usize>,
    },
    Spec(Rc<RefCell<ResolvedSpecType>>),
    SpecObject {
        spec: Rc<RefCell<ResolvedSpecType>>,
        type_args: Vec<ResolvedType>,
        mutable: bool,
    },
}

impl Hash for ResolvedType {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            Self::Void
            | Self::Never
            | Self::Bool
            | Self::Char
            | Self::I8
            | Self::I16
            | Self::I32
            | Self::I64
            | Self::ISize
            | Self::U8
            | Self::U16
            | Self::U32
            | Self::U64
            | Self::USize
            | Self::F32
            | Self::F64 => {}
            Self::Array(inner, mutable) => {
                inner.hash(state);
                mutable.hash(state);
            }
            Self::Pointer { pointee, mutable } => {
                pointee.hash(state);
                mutable.hash(state);
            }
            Self::Slice { item, mutable } => {
                item.hash(state);
                mutable.hash(state);
            }
            Self::Str { mutable } => mutable.hash(state),
            Self::Function(fn_type) => fn_type.hash(state),
            Self::SizedArray(inner, size) => {
                inner.hash(state);
                size.hash(state);
            }
            Self::Struct(cell) => cell.borrow().hash(state),
            Self::Union(cell) => cell.borrow().hash(state),
            Self::Enum { cell, variant } => {
                cell.borrow().hash(state);
                variant.hash(state);
            }
            Self::Spec(cell) => cell.borrow().hash(state),
            Self::SpecObject {
                spec,
                type_args,
                mutable,
            } => {
                spec.borrow().hash(state);
                type_args.hash(state);
                mutable.hash(state);
            }
        }
    }
}

impl std::fmt::Display for ResolvedType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Void => write!(f, "void"),
            Self::Never => write!(f, "never"),
            Self::Bool => write!(f, "bool"),
            Self::Char => write!(f, "char"),
            Self::I8 => write!(f, "i8"),
            Self::I16 => write!(f, "i16"),
            Self::I32 => write!(f, "i32"),
            Self::I64 => write!(f, "i64"),
            Self::ISize => write!(f, "isize"),
            Self::U8 => write!(f, "u8"),
            Self::U16 => write!(f, "u16"),
            Self::U32 => write!(f, "u32"),
            Self::U64 => write!(f, "u64"),
            Self::USize => write!(f, "usize"),
            Self::F32 => write!(f, "f32"),
            Self::F64 => write!(f, "f64"),
            Self::Pointer {
                pointee,
                mutable: false,
            } => write!(f, "*{pointee}"),
            Self::Pointer {
                pointee,
                mutable: true,
            } => write!(f, "*mut {pointee}"),
            Self::Function(fn_type) => {
                write!(f, "(")?;
                for (i, (name, param)) in fn_type.params.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    if name.as_ref().is_empty() {
                        write!(f, "{param}")?;
                    } else {
                        write!(f, "{}: {param}", name.as_ref())?;
                    }
                }
                if fn_type.is_variadic {
                    if !fn_type.params.is_empty() {
                        write!(f, ", ")?;
                    }
                    write!(f, "...")?;
                }
                write!(f, ") => {}", fn_type.return_type)
            }
            Self::Array(inner, false) => write!(f, "*[?]{inner}"),
            Self::Array(inner, true) => write!(f, "*mut [?]{inner}"),
            Self::SizedArray(inner, size) => write!(f, "[{size}]{inner}"),
            Self::Slice {
                item,
                mutable: false,
            } => write!(f, "*[]{item}"),
            Self::Slice {
                item,
                mutable: true,
            } => write!(f, "*mut []{item}"),
            Self::Str { mutable: false } => write!(f, "*str"),
            Self::Str { mutable: true } => write!(f, "*mut str"),
            Self::Struct(cell) => write!(f, "{}", cell.borrow().name.as_ref()),
            Self::Union(cell) => write!(f, "{}", cell.borrow().name.as_ref()),
            Self::Enum { cell, variant } => {
                let e = cell.borrow();
                write!(f, "{}", e.name.as_ref())?;
                if let Some(index) = variant {
                    write!(f, "::{}", e.variants[*index].name.as_ref())?;
                }
                Ok(())
            }
            Self::Spec(cell) => write!(f, "{}", cell.borrow().name.as_ref()),
            Self::SpecObject {
                spec,
                type_args,
                mutable,
            } => {
                write!(f, "spec *{}", if *mutable { "mut " } else { "" })?;
                write!(f, "{}", spec.borrow().name.as_ref())?;
                if !type_args.is_empty() {
                    write!(f, "<")?;
                    for (i, arg) in type_args.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{arg}")?;
                    }
                    write!(f, ">")?;
                }
                Ok(())
            }
        }
    }
}

impl ResolvedType {
    pub fn numeric_kind(&self, pointer_bits: u32) -> Option<NumericKind> {
        Some(match self {
            Self::I8 => NumericKind::Signed(8),
            Self::I16 => NumericKind::Signed(16),
            Self::I32 => NumericKind::Signed(32),
            Self::I64 => NumericKind::Signed(64),
            Self::ISize => NumericKind::Signed(pointer_bits),
            Self::U8 => NumericKind::Unsigned(8),
            Self::U16 => NumericKind::Unsigned(16),
            Self::U32 => NumericKind::Unsigned(32),
            Self::U64 => NumericKind::Unsigned(64),
            Self::USize => NumericKind::Unsigned(pointer_bits),
            Self::F32 => NumericKind::Float(32),
            Self::F64 => NumericKind::Float(64),
            _ => return None,
        })
    }

    pub fn arithmetic_repr(&self) -> Option<ResolvedType> {
        match self {
            Self::Pointer { .. } => Some(ResolvedType::USize),
            _ => None,
        }
    }

    pub fn primitive_byte_size(&self, pointer_bytes: u32) -> Option<u32> {
        match self {
            Self::Bool => Some(1),
            Self::Char => Some(4),
            Self::Pointer { .. } => Some(pointer_bytes),
            _ => self.numeric_kind(pointer_bytes * 8).map(|kind| {
                let width = match kind {
                    NumericKind::Signed(w) | NumericKind::Unsigned(w) | NumericKind::Float(w) => w,
                };
                width / 8
            }),
        }
    }

    pub fn cast_class(&self, pointer_bits: u32) -> Option<CastClass> {
        if let Some(kind) = self.numeric_kind(pointer_bits) {
            return Some(match kind {
                NumericKind::Signed(width) => CastClass::Int {
                    width,
                    signed: true,
                },
                NumericKind::Unsigned(width) => CastClass::Int {
                    width,
                    signed: false,
                },
                NumericKind::Float(width) => CastClass::Float { width },
            });
        }
        match self {
            Self::Pointer { .. } => Some(CastClass::Int {
                width: pointer_bits,
                signed: false,
            }),
            Self::Char => Some(CastClass::Int {
                width: 32,
                signed: false,
            }),
            Self::Bool => Some(CastClass::Int {
                width: 8,
                signed: false,
            }),
            _ => None,
        }
    }

    pub fn integer_domain(&self, pointer_bits: u32) -> Option<(i128, i128)> {
        Some(match self {
            Self::Bool => (0, 1),
            Self::Char => (0, char::MAX as i128),
            Self::I8 => (i8::MIN as i128, i8::MAX as i128),
            Self::I16 => (i16::MIN as i128, i16::MAX as i128),
            Self::I32 => (i32::MIN as i128, i32::MAX as i128),
            Self::I64 => (i64::MIN as i128, i64::MAX as i128),
            Self::ISize if pointer_bits == 32 => (i32::MIN as i128, i32::MAX as i128),
            Self::ISize => (i64::MIN as i128, i64::MAX as i128),
            Self::U8 => (u8::MIN as i128, u8::MAX as i128),
            Self::U16 => (u16::MIN as i128, u16::MAX as i128),
            Self::U32 => (u32::MIN as i128, u32::MAX as i128),
            Self::U64 => (u64::MIN as i128, u64::MAX as i128),
            Self::USize if pointer_bits == 32 => (u32::MIN as i128, u32::MAX as i128),
            Self::USize => (u64::MIN as i128, u64::MAX as i128),
            _ => return None,
        })
    }

    pub fn widened(&self) -> ResolvedType {
        match self {
            Self::Enum {
                cell,
                variant: Some(_),
            } => Self::Enum {
                cell: cell.clone(),
                variant: None,
            },
            other => other.clone(),
        }
    }

    pub fn lookup_key(&self) -> ResolvedType {
        match self {
            Self::Enum { cell, .. } => Self::Enum {
                cell: cell.clone(),
                variant: None,
            },
            Self::Pointer { pointee, .. } => Self::Pointer {
                pointee: Box::new(pointee.lookup_key()),
                mutable: false,
            },
            Self::Slice { item, .. } => Self::Slice {
                item: Box::new(item.lookup_key()),
                mutable: false,
            },
            Self::Str { .. } => Self::Str { mutable: false },
            other => other.clone(),
        }
    }

    pub fn accepts(&self, value: &ResolvedType) -> bool {
        if self == value {
            return true;
        }
        match (self, value) {
            (
                Self::Enum {
                    cell: expected,
                    variant: None,
                },
                Self::Enum {
                    cell: found,
                    variant: Some(_),
                },
            ) => expected.borrow().id == found.borrow().id,
            (
                Self::Pointer {
                    pointee: expected,
                    mutable: false,
                },
                Self::Pointer { pointee: found, .. },
            ) => expected.accepts(found),
            (
                Self::Slice {
                    item: expected,
                    mutable: false,
                },
                Self::Slice { item: found, .. },
            ) => expected.accepts(found),
            (Self::Array(expected, false), Self::Array(found, _)) => expected.accepts(found),
            // No `item` to recurse on, unlike `Slice` above -- and
            // deliberately its own arm, not folded into `Slice`'s: `*str`
            // and `*[u8]` must never accept one another implicitly (see
            // `Str`'s own doc comment), only `*mut str` -> `*str` widening.
            (Self::Str { mutable: false }, Self::Str { .. }) => true,
            _ => false,
        }
    }

    pub fn declared_methods(&self) -> Option<Vec<(Ident, ResolvedMethod)>> {
        match self {
            Self::Struct(cell) => Some(cell.borrow().functions.clone()),
            Self::Union(cell) => Some(cell.borrow().functions.clone()),
            Self::Enum { cell, .. } => Some(cell.borrow().functions.clone()),
            _ => None,
        }
    }

    pub fn declaring_owner(&self) -> Option<(Vec<Ident>, HirId)> {
        match self {
            Self::Struct(cell) => Some((cell.borrow().module_path.clone(), cell.borrow().id)),
            Self::Union(cell) => Some((cell.borrow().module_path.clone(), cell.borrow().id)),
            Self::Enum { cell, .. } => Some((cell.borrow().module_path.clone(), cell.borrow().id)),
            _ => None,
        }
    }

    pub fn autoderef(&self) -> &ResolvedType {
        match self {
            Self::Pointer { pointee, .. } => pointee,
            other => other,
        }
    }

    pub fn pointer_like_mutable(&self) -> Option<bool> {
        match self {
            Self::Pointer { mutable, .. } | Self::Slice { mutable, .. } | Self::Str { mutable } => {
                Some(*mutable)
            }
            Self::Array(_, mutable) => Some(*mutable),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests;
