use omega_hir::{HirBlock, HirGenericParam, HirId, HirParam};
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

/// A resolved function-type parameter. `name` is the optional descriptor
/// written at the source level, or a declaration's binding name carried
/// through for presentation. It is metadata only: equality and hashing --
/// and therefore function-type identity -- depend on `r#type` alone. See
/// `docs/architecture/types-layout-and-const-eval.md`.
#[derive(Debug, Clone)]
pub struct ResolvedFunctionParam {
    pub name: Option<Ident>,
    pub r#type: ResolvedType,
}

impl ResolvedFunctionParam {
    pub fn described(name: Ident, r#type: ResolvedType) -> Self {
        Self {
            name: Some(name),
            r#type,
        }
    }

    pub fn anonymous(r#type: ResolvedType) -> Self {
        Self { name: None, r#type }
    }
}

impl PartialEq for ResolvedFunctionParam {
    fn eq(&self, other: &Self) -> bool {
        self.r#type == other.r#type
    }
}

impl Eq for ResolvedFunctionParam {}

impl Hash for ResolvedFunctionParam {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.r#type.hash(state);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ResolvedFunctionType {
    pub params: Vec<ResolvedFunctionParam>,
    pub return_type: Box<ResolvedType>,
    pub is_variadic: bool,
    pub self_mode: Option<SelfMode>,
    pub calling_convention: CallingConvention,
}

impl ResolvedFunctionType {
    pub fn param_types(&self) -> impl Iterator<Item = &ResolvedType> {
        self.params.iter().map(|param| &param.r#type)
    }

    /// This declaration's type as an unbound first-class function value.
    ///
    /// HIR lowering already materialized a receiver as parameter 0, so the
    /// value view only drops the declaration-only receiver metadata: the
    /// remaining signature is the real ABI the address was compiled with.
    /// The receiver's descriptor is dropped with it, because the receiver is
    /// now an ordinary explicit argument rather than a name the callee's own
    /// body reaches through.
    pub fn unbound_value(&self) -> Self {
        let mut value = self.clone();
        if value.self_mode.take().is_some()
            && let Some(receiver) = value.params.first_mut()
        {
            receiver.name = None;
        }
        value
    }

    /// Whether a value of type `value` may be used where `self` is expected.
    /// Function types are invariant, and parameter descriptors are not part
    /// of their identity, so this is exactly equality.
    pub fn accepts(&self, value: &Self) -> bool {
        self == value
    }
}

/// Which associated-function namespace a type-qualified path selects.
///
/// The two namespaces are independent: a receiverless and a receiver-bearing
/// function may share a name and a parameter list without colliding, and
/// neither namespace can be reached with the other's spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FunctionNamespace {
    /// `Type::name` -- receiverless associated functions.
    Static,
    /// `Type::self::name` -- functions declaring `self`, `mut self`, `*self`,
    /// or `*mut self`.
    Member,
}

impl FunctionNamespace {
    /// The contextual segment selecting [`Self::Member`]. It only has this
    /// meaning directly after a resolved type prefix and only when a further
    /// segment follows it, so a leading module-relative `self::...` and a
    /// static function literally named `self` both keep their meaning.
    pub const MEMBER_SEGMENT: &'static str = "self";

    pub fn of(fn_type: &ResolvedFunctionType) -> Self {
        Self::of_declaration(fn_type.self_mode)
    }

    /// The namespace a declaration's written receiver form places it in.
    pub fn of_declaration(self_mode: Option<SelfMode>) -> Self {
        match self_mode {
            Some(_) => Self::Member,
            None => Self::Static,
        }
    }

    pub fn other(self) -> Self {
        match self {
            Self::Static => Self::Member,
            Self::Member => Self::Static,
        }
    }

    /// The declarations named `name` that belong to this namespace, in
    /// declaration order.
    pub fn select(
        self,
        functions: &[(Ident, ResolvedMethod)],
        name: &Ident,
    ) -> Vec<ResolvedMethod> {
        functions
            .iter()
            .filter(|(candidate, method)| candidate == name && method.namespace() == self)
            .map(|(_, method)| method.clone())
            .collect()
    }

    /// The names this namespace offers, for "did you mean" reporting.
    pub fn names(self, functions: &[(Ident, ResolvedMethod)]) -> Vec<&Ident> {
        functions
            .iter()
            .filter(|(_, method)| method.namespace() == self)
            .map(|(name, _)| name)
            .collect()
    }

    /// How a path selecting this namespace is written.
    pub fn spelling(self, owner: &str, name: &Ident) -> String {
        match self {
            Self::Static => format!("{owner}::{name}"),
            Self::Member => format!("{owner}::{}::{name}", Self::MEMBER_SEGMENT),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedMethod {
    pub decl_id: HirId,
    pub fn_type: ResolvedFunctionType,
    pub visibility: Visibility,
    pub annotations: crate::annotations::ResolvedAnnotations,
    pub source: Option<ConformanceSource>,
}

impl ResolvedMethod {
    pub fn namespace(&self) -> FunctionNamespace {
        FunctionNamespace::of(&self.fn_type)
    }

    /// This function's type when it is acquired by address rather than
    /// called through its owner. See [`ResolvedFunctionType::unbound_value`].
    pub fn value_fn_type(&self) -> ResolvedFunctionType {
        self.fn_type.unbound_value()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConformanceSource {
    pub spec: Rc<RefCell<ResolvedSpecType>>,
    pub spec_args: Vec<ResolvedGenericArg>,
}

#[derive(Debug, Clone)]
pub struct ResolvedConformance {
    pub target: ResolvedType,
    pub spec: Rc<RefCell<ResolvedSpecType>>,
    pub spec_args: Vec<ResolvedGenericArg>,
    pub methods: Vec<(Ident, ResolvedMethod)>,
}

#[derive(Debug, Clone)]
pub struct ResolvedBound {
    pub target: ResolvedType,
    pub spec: Rc<RefCell<ResolvedSpecType>>,
    pub spec_args: Vec<ResolvedGenericArg>,
}

impl ResolvedBound {
    pub fn new(
        target: ResolvedType,
        spec: Rc<RefCell<ResolvedSpecType>>,
        spec_args: Vec<ResolvedGenericArg>,
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
    pub generic_args: Vec<ResolvedGenericArg>,
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
    pub generic_args: Vec<ResolvedGenericArg>,
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
    pub generic_args: Vec<ResolvedGenericArg>,
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
    pub generics: Vec<omega_hir::HirGenericParam>,
    pub module_path: Vec<Ident>,
    pub generic_args: Vec<ResolvedGenericArg>,
    pub is_object_safe: bool,
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

/// One resolved member of a spec conjunction: a spec declaration plus its
/// resolved (already-substituted) generic arguments. `ResolvedSpecType`
/// itself never carries the arguments of a particular application -- the
/// declaration is cached once and shared, so every application pairs it with
/// its own argument list here instead.
#[derive(Debug, Clone)]
pub struct ResolvedSpecApplication {
    pub spec: Rc<RefCell<ResolvedSpecType>>,
    pub spec_args: Vec<ResolvedGenericArg>,
}

impl ResolvedSpecApplication {
    pub fn new(spec: Rc<RefCell<ResolvedSpecType>>, spec_args: Vec<ResolvedGenericArg>) -> Self {
        Self { spec, spec_args }
    }

    /// The deterministic ordering/dedup key. Shared with anonymous-enum
    /// canonicalization so one notion of structural identity orders every
    /// unordered type set; see `crate::type_key`.
    fn canonical_key(&self) -> String {
        crate::type_key::spec_application_key(self)
    }
}

impl PartialEq for ResolvedSpecApplication {
    fn eq(&self, other: &Self) -> bool {
        self.spec.borrow().id == other.spec.borrow().id && self.spec_args == other.spec_args
    }
}
impl Eq for ResolvedSpecApplication {}

impl Hash for ResolvedSpecApplication {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.spec.borrow().hash(state);
        self.spec_args.hash(state);
    }
}

impl std::fmt::Display for ResolvedSpecApplication {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.spec.borrow().name.as_ref())?;
        if !self.spec_args.is_empty() {
            write!(f, "<")?;
            for (i, arg) in self.spec_args.iter().enumerate() {
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

/// A canonical spec conjunction: a deduplicated, deterministically ordered
/// list of resolved spec applications. Commutative and idempotent at the
/// semantic level -- `A + B` and `B + A` normalize to the same shape, and
/// `A + A` normalizes to `A` -- so source order never participates in
/// equality, hashing, mangling, or vtable identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ResolvedSpecShape {
    pub members: Vec<ResolvedSpecApplication>,
}

impl ResolvedSpecShape {
    /// Sorts by canonical key and removes exact duplicate applications.
    /// Callers are responsible for resolving every member to its final spec
    /// declaration/normalized arguments before calling this.
    pub fn canonicalize(mut members: Vec<ResolvedSpecApplication>) -> Self {
        members.sort_by(|a, b| a.canonical_key().cmp(&b.canonical_key()));
        members.dedup();
        Self { members }
    }
}

impl std::fmt::Display for ResolvedSpecShape {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (i, member) in self.members.iter().enumerate() {
            if i > 0 {
                write!(f, " + ")?;
            }
            write!(f, "{member}")?;
        }
        Ok(())
    }
}

/// The canonical member list of an anonymous enum.
///
/// Members are flattened to leaves, ordered by `type_key::structural_key`, and
/// exact duplicates are removed, so `enum A | B`, `enum B | A`,
/// `enum A | B | A`, and `enum (enum A | B) | B` are one type with one layout,
/// one tag assignment, and one mangled symbol. Construction goes through
/// `canonicalize` precisely so no later phase can observe source order or a
/// nested member.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ResolvedAnonymousEnum {
    members: Vec<ResolvedType>,
}

impl ResolvedAnonymousEnum {
    /// The tag is a `u32` holding the canonical member index, so a shape can
    /// have at most this many distinct members.
    pub const MAX_MEMBERS: u64 = u32::MAX as u64 + 1;

    /// Callers must resolve every member to its final type first; ordering
    /// and deduplication are meaningless on unresolved syntax.
    ///
    /// The stored list holds no `ResolvedType::AnonymousEnum`: an immediate
    /// anonymous-enum member is replaced by its own members, so an alias or a
    /// generic substitution that lands one anonymous enum inside another
    /// produces the same type as writing the leaves directly. Every other
    /// constructor is a boundary and stays one atomic member, including a
    /// named enum and a pointer/array/function/generic type that merely
    /// *contains* an anonymous enum.
    pub fn canonicalize(members: Vec<ResolvedType>) -> Self {
        let mut flattened = Vec::with_capacity(members.len());
        for member in members {
            Self::flatten_member_into(member, &mut flattened);
        }
        flattened.sort_by_cached_key(crate::type_key::structural_key);
        flattened.dedup();
        Self { members: flattened }
    }

    /// Refinement proves which member a value currently holds; it is not part
    /// of type or storage identity, so a refined member contributes its whole
    /// shape here rather than the single proven member.
    fn flatten_member_into(member: ResolvedType, out: &mut Vec<ResolvedType>) {
        match member {
            ResolvedType::AnonymousEnum { shape, .. } => {
                for inner in shape.members() {
                    Self::flatten_member_into(inner.clone(), out);
                }
            }
            atomic => out.push(atomic),
        }
    }

    pub fn members(&self) -> &[ResolvedType] {
        &self.members
    }

    /// The canonical index -- and therefore the tag -- of an exact member.
    pub fn index_of(&self, member: &ResolvedType) -> Option<usize> {
        self.members
            .iter()
            .position(|candidate| candidate == member)
    }

    /// The destination index of every `source` member, in source canonical
    /// order, when every one of them is a member here.
    ///
    /// Canonical order is a total order over structural keys rather than an
    /// extension of the source's, so a destination-only member can sort
    /// before a shared one and shift every later index. A caller therefore
    /// has to retag through this map; the source tag is never reusable.
    pub fn subset_remap(&self, source: &Self) -> Option<Vec<usize>> {
        source
            .members
            .iter()
            .map(|member| self.index_of(member))
            .collect()
    }

    /// Whether the canonical member list outgrew the fixed `u32` tag. Checked
    /// once, at type resolution, so no later phase can meet a shape it cannot
    /// tag.
    pub fn exceeds_tag_domain(&self) -> bool {
        self.members.len() as u64 > Self::MAX_MEMBERS
    }
}

impl std::fmt::Display for ResolvedAnonymousEnum {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "enum ")?;
        for (i, member) in self.members.iter().enumerate() {
            if i > 0 {
                write!(f, " | ")?;
            }
            write!(f, "{member}")?;
        }
        Ok(())
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
    pub generics: Vec<HirGenericParam>,
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
    /// Who may *call* this function through an ordinary path. Glue matching
    /// is not a call, so this is deliberately not part of the gap's ABI or
    /// conformance identity.
    pub visibility: Visibility,
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

impl ConstValue {
    /// An anonymous enum's constant form: the tag is the canonical member
    /// index, and the shape has no header or dynamic field to fill.
    pub fn anonymous_enum(index: usize, fields: Vec<ConstValue>) -> Self {
        Self::Enum {
            variant_index: index,
            tag: crate::checked::NumberValue::Unsigned(index as u64),
            header: Vec::new(),
            dynamic_fields: Vec::new(),
            fields,
        }
    }
}

/// The integer primitives a `comp` generic parameter may be declared with.
/// Keeping the set closed here is what gives every comp argument a stable
/// cross-query equality, hash, and mangling; widening it is a deliberate
/// language decision, not an implementation detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompIntType {
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

impl CompIntType {
    pub fn from_resolved(r#type: &ResolvedType) -> Option<Self> {
        Some(match r#type {
            ResolvedType::I8 => Self::I8,
            ResolvedType::I16 => Self::I16,
            ResolvedType::I32 => Self::I32,
            ResolvedType::I64 => Self::I64,
            ResolvedType::ISize => Self::ISize,
            ResolvedType::U8 => Self::U8,
            ResolvedType::U16 => Self::U16,
            ResolvedType::U32 => Self::U32,
            ResolvedType::U64 => Self::U64,
            ResolvedType::USize => Self::USize,
            _ => return None,
        })
    }

    pub fn resolved(self) -> ResolvedType {
        match self {
            Self::I8 => ResolvedType::I8,
            Self::I16 => ResolvedType::I16,
            Self::I32 => ResolvedType::I32,
            Self::I64 => ResolvedType::I64,
            Self::ISize => ResolvedType::ISize,
            Self::U8 => ResolvedType::U8,
            Self::U16 => ResolvedType::U16,
            Self::U32 => ResolvedType::U32,
            Self::U64 => ResolvedType::U64,
            Self::USize => ResolvedType::USize,
        }
    }

    pub fn is_signed(self) -> bool {
        matches!(
            self,
            Self::I8 | Self::I16 | Self::I32 | Self::I64 | Self::ISize
        )
    }
}

/// The full set of types a `comp` generic parameter may currently be
/// declared with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompScalarType {
    Int(CompIntType),
    Bool,
    Char,
}

impl CompScalarType {
    pub fn from_resolved(r#type: &ResolvedType) -> Option<Self> {
        match r#type {
            ResolvedType::Bool => Some(Self::Bool),
            ResolvedType::Char => Some(Self::Char),
            other => CompIntType::from_resolved(other).map(Self::Int),
        }
    }

    pub fn resolved(self) -> ResolvedType {
        match self {
            Self::Int(int) => int.resolved(),
            Self::Bool => ResolvedType::Bool,
            Self::Char => ResolvedType::Char,
        }
    }
}

/// A canonical compile-time value bound to a `comp` generic parameter. The
/// declared type is part of the value's identity: the same digits under
/// `comp N: usize` and `comp N: u8` are different instantiations, so they
/// must not share a query entry or a symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompScalar {
    Int { r#type: CompIntType, value: i128 },
    Bool(bool),
    Char(char),
}

impl CompScalar {
    pub fn resolved_type(&self) -> ResolvedType {
        match self {
            Self::Int { r#type, .. } => r#type.resolved(),
            Self::Bool(_) => ResolvedType::Bool,
            Self::Char(_) => ResolvedType::Char,
        }
    }

    pub fn as_int(&self) -> Option<i128> {
        match self {
            Self::Int { value, .. } => Some(*value),
            _ => None,
        }
    }

    /// The ordinary compile-time value this binding hands to the rest of the
    /// analyzer, so a reference to a comp generic behaves exactly like a
    /// reference to any other `comp` binding.
    pub fn const_value(&self) -> ConstValue {
        match self {
            Self::Int { r#type, value } if r#type.is_signed() => {
                ConstValue::Number(crate::checked::NumberValue::Signed(*value as i64))
            }
            Self::Int { value, .. } => {
                ConstValue::Number(crate::checked::NumberValue::Unsigned(*value as u64))
            }
            Self::Bool(value) => ConstValue::Bool(*value),
            Self::Char(value) => ConstValue::Char(*value),
        }
    }

    /// Canonicalizes an already-evaluated compile-time value against the
    /// declared parameter type, which is authoritative. An integer of any
    /// compile-time integer type is accepted when it is exactly
    /// representable there; nothing is truncated, wrapped, or implicitly
    /// converted across kinds.
    pub fn normalize(
        value: &ConstValue,
        declared: CompScalarType,
        pointer_bits: u32,
    ) -> Option<Self> {
        match (declared, value) {
            (CompScalarType::Int(r#type), ConstValue::Number(number)) => {
                let value = match number {
                    crate::checked::NumberValue::Signed(v) => i128::from(*v),
                    crate::checked::NumberValue::Unsigned(v) => i128::from(*v),
                    crate::checked::NumberValue::Float(_) => return None,
                };
                let (min, max) = r#type.resolved().integer_domain(pointer_bits)?;
                (min..=max)
                    .contains(&value)
                    .then_some(Self::Int { r#type, value })
            }
            (CompScalarType::Bool, ConstValue::Bool(value)) => Some(Self::Bool(*value)),
            (CompScalarType::Char, ConstValue::Char(value)) => Some(Self::Char(*value)),
            _ => None,
        }
    }
}

impl std::fmt::Display for CompScalar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Int { value, .. } => write!(f, "{value}"),
            Self::Bool(value) => write!(f, "{value}"),
            Self::Char(value) => write!(f, "'{value}'"),
        }
    }
}

/// One resolved generic argument. Generic argument lists are ordered and
/// mixed: a position is a type or a compile-time value according to the
/// declared parameter's kind, and both participate in type identity, query
/// identity, and symbol identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ResolvedGenericArg {
    Type(ResolvedType),
    Comp(CompScalar),
}

impl ResolvedGenericArg {
    pub fn as_type(&self) -> Option<&ResolvedType> {
        match self {
            Self::Type(r#type) => Some(r#type),
            Self::Comp(_) => None,
        }
    }

    pub fn as_comp(&self) -> Option<CompScalar> {
        match self {
            Self::Comp(value) => Some(*value),
            Self::Type(_) => None,
        }
    }

    pub fn is_comp(&self) -> bool {
        matches!(self, Self::Comp(_))
    }

    pub fn widened(&self) -> Self {
        match self {
            Self::Type(r#type) => Self::Type(r#type.widened()),
            Self::Comp(value) => Self::Comp(*value),
        }
    }

    fn nested(&self, qualified: bool) -> String {
        match self {
            Self::Type(r#type) => r#type.nested(qualified),
            Self::Comp(value) => value.to_string(),
        }
    }
}

impl From<ResolvedType> for ResolvedGenericArg {
    fn from(r#type: ResolvedType) -> Self {
        Self::Type(r#type)
    }
}

impl std::fmt::Display for ResolvedGenericArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Type(r#type) => write!(f, "{type}"),
            Self::Comp(value) => write!(f, "{value}"),
        }
    }
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
        shape: ResolvedSpecShape,
        mutable: bool,
    },
    /// A structural `enum A | B | ...`. `variant` mirrors `Enum`'s: it is a
    /// lexical proof that the value currently holds that canonical member,
    /// never a change of storage or ABI.
    AnonymousEnum {
        shape: Rc<ResolvedAnonymousEnum>,
        variant: Option<usize>,
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
            Self::SpecObject { shape, mutable } => {
                shape.hash(state);
                mutable.hash(state);
            }
            Self::AnonymousEnum { shape, variant } => {
                shape.hash(state);
                variant.hash(state);
            }
        }
    }
}

fn self_mode_spelling(self_mode: SelfMode) -> &'static str {
    match self_mode {
        SelfMode::Value => "self",
        SelfMode::MutValue => "mut self",
        SelfMode::Pointer => "*self",
        SelfMode::MutPointer => "*mut self",
    }
}

impl std::fmt::Display for ResolvedType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.write(f, false)
    }
}

/// A rendering that spells nominal types with their declaring module path.
/// Diagnostics fall back to it when two unequal types would otherwise print
/// the same short name.
pub struct QualifiedType<'a>(pub &'a ResolvedType);

impl std::fmt::Display for QualifiedType<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.write(f, true)
    }
}

fn write_module_path(f: &mut std::fmt::Formatter<'_>, module_path: &[Ident]) -> std::fmt::Result {
    for segment in module_path {
        write!(f, "{}::", segment.as_ref())?;
    }
    Ok(())
}

impl ResolvedType {
    fn nested(&self, qualified: bool) -> String {
        if qualified {
            QualifiedType(self).to_string()
        } else {
            self.to_string()
        }
    }

    fn write_generic_args(
        f: &mut std::fmt::Formatter<'_>,
        args: &[ResolvedGenericArg],
        qualified: bool,
    ) -> std::fmt::Result {
        if args.is_empty() {
            return Ok(());
        }
        let rendered: Vec<String> = args.iter().map(|arg| arg.nested(qualified)).collect();
        write!(f, "<{}>", rendered.join(", "))
    }

    fn write(&self, f: &mut std::fmt::Formatter<'_>, qualified: bool) -> std::fmt::Result {
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
            } => write!(f, "*{}", pointee.nested(qualified)),
            Self::Pointer {
                pointee,
                mutable: true,
            } => write!(f, "*mut {}", pointee.nested(qualified)),
            Self::Function(fn_type) => {
                if fn_type.calling_convention != CallingConvention::Omega {
                    write!(f, "foreign({}) ", fn_type.calling_convention)?;
                }
                write!(f, "(")?;
                let mut wrote_param = false;
                if let Some(self_mode) = fn_type.self_mode {
                    write!(f, "{}", self_mode_spelling(self_mode))?;
                    wrote_param = true;
                }
                for param in &fn_type.params {
                    if wrote_param {
                        write!(f, ", ")?;
                    }
                    match &param.name {
                        Some(name) => write!(f, "{name}: {}", param.r#type.nested(qualified))?,
                        None => write!(f, "{}", param.r#type.nested(qualified))?,
                    }
                    wrote_param = true;
                }
                if fn_type.is_variadic {
                    if wrote_param {
                        write!(f, ", ")?;
                    }
                    write!(f, "...")?;
                }
                write!(f, ") => {}", fn_type.return_type.nested(qualified))
            }
            Self::Array(inner, false) => write!(f, "*[?]{}", inner.nested(qualified)),
            Self::Array(inner, true) => write!(f, "*mut [?]{}", inner.nested(qualified)),
            Self::SizedArray(inner, size) => write!(f, "[{size}]{}", inner.nested(qualified)),
            Self::Slice {
                item,
                mutable: false,
            } => write!(f, "*[]{}", item.nested(qualified)),
            Self::Slice {
                item,
                mutable: true,
            } => write!(f, "*mut []{}", item.nested(qualified)),
            Self::Str { mutable: false } => write!(f, "*str"),
            Self::Str { mutable: true } => write!(f, "*mut str"),
            Self::Struct(cell) => {
                let s = cell.borrow();
                if qualified {
                    write_module_path(f, &s.module_path)?;
                }
                write!(f, "{}", s.name.as_ref())?;
                Self::write_generic_args(f, &s.generic_args, qualified)
            }
            Self::Union(cell) => {
                let u = cell.borrow();
                if qualified {
                    write_module_path(f, &u.module_path)?;
                }
                write!(f, "{}", u.name.as_ref())?;
                Self::write_generic_args(f, &u.generic_args, qualified)
            }
            Self::Enum { cell, variant } => {
                let e = cell.borrow();
                if qualified {
                    write_module_path(f, &e.module_path)?;
                }
                write!(f, "{}", e.name.as_ref())?;
                Self::write_generic_args(f, &e.generic_args, qualified)?;
                if let Some(index) = variant {
                    write!(f, "::{}", e.variants[*index].name.as_ref())?;
                }
                Ok(())
            }
            Self::Spec(cell) => {
                let sp = cell.borrow();
                if qualified {
                    write_module_path(f, &sp.module_path)?;
                }
                write!(f, "{}", sp.name.as_ref())?;
                Self::write_generic_args(f, &sp.generic_args, qualified)
            }
            Self::SpecObject { shape, mutable } => {
                write!(f, "*{}spec {shape}", if *mutable { "mut " } else { "" })
            }
            Self::AnonymousEnum { shape, variant } => match variant {
                Some(index) => write!(f, "{} ({})", shape.members()[*index], shape),
                None => write!(f, "{shape}"),
            },
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
            Self::ISize => (
                -(1i128 << (pointer_bits - 1)),
                (1i128 << (pointer_bits - 1)) - 1,
            ),
            Self::U8 => (u8::MIN as i128, u8::MAX as i128),
            Self::U16 => (u16::MIN as i128, u16::MAX as i128),
            Self::U32 => (u32::MIN as i128, u32::MAX as i128),
            Self::U64 => (u64::MIN as i128, u64::MAX as i128),
            Self::USize => (0, (1i128 << pointer_bits) - 1),
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
            Self::AnonymousEnum {
                shape,
                variant: Some(_),
            } => Self::AnonymousEnum {
                shape: shape.clone(),
                variant: None,
            },
            other => other.clone(),
        }
    }

    /// The member an anonymous-enum refinement proves, if this type is one.
    /// Reading a refined binding, projecting a field off it, or calling a
    /// method on it all go through this view, while the value's storage stays
    /// the whole anonymous enum.
    pub fn refined_anonymous_member(&self) -> Option<(usize, &ResolvedType)> {
        match self {
            Self::AnonymousEnum {
                shape,
                variant: Some(index),
            } => Some((*index, &shape.members()[*index])),
            _ => None,
        }
    }

    pub fn lookup_key(&self) -> ResolvedType {
        match self {
            Self::Enum { cell, .. } => Self::Enum {
                cell: cell.clone(),
                variant: None,
            },
            Self::AnonymousEnum { shape, .. } => Self::AnonymousEnum {
                shape: shape.clone(),
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
            // Refinement is not part of the representation, so dropping it is
            // a plain copy. Widening between *different* anonymous shapes is
            // deliberately absent here: canonical indices and payload size can
            // both differ, so it is a real conversion that has to leave a node
            // behind, not an acceptance rule.
            (
                Self::AnonymousEnum {
                    shape: expected,
                    variant: None,
                },
                Self::AnonymousEnum {
                    shape: found,
                    variant: Some(_),
                },
            ) => expected == found,
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
            (Self::Function(expected), Self::Function(found)) => expected.accepts(found),
            _ => false,
        }
    }

    /// Every function this nominal type declares, across both namespaces, or
    /// `None` for a type that owns no declarations of its own. Deliberately
    /// private: semantic lookup goes through [`Self::candidates_in`], so no
    /// path searches a mixed set and rejects the wrong receiver kind after
    /// selecting from it.
    fn declared_functions(&self) -> Option<Vec<(Ident, ResolvedMethod)>> {
        match self {
            Self::Struct(cell) => Some(cell.borrow().functions.clone()),
            Self::Union(cell) => Some(cell.borrow().functions.clone()),
            Self::Enum { cell, .. } => Some(cell.borrow().functions.clone()),
            _ => None,
        }
    }

    /// The declarations of `name` this owner offers in `namespace`, in
    /// declaration order.
    pub fn candidates_in(
        &self,
        namespace: FunctionNamespace,
        name: &Ident,
    ) -> Option<Vec<ResolvedMethod>> {
        Some(namespace.select(&self.declared_functions()?, name))
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
