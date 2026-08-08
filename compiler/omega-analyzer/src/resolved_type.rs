use omega_hir::{HirBlock, HirId, HirParam};
use omega_parser::prelude::{Ident, SelfMode, Span, Type, Visibility};
use std::cell::RefCell;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ResolvedFunctionType {
    pub params: Vec<(Ident, ResolvedType)>,
    pub return_type: Box<ResolvedType>,
    pub is_variadic: bool,
    /// `None` for an ordinary function; `Some` for a member function,
    /// carrying exactly how it receives `self` -- see `SelfMode`. The
    /// single source of truth for self's passing convention at any call
    /// site; never reverse-engineered from `params[0]`'s resolved type
    /// shape.
    pub self_mode: Option<SelfMode>,
}

/// A struct method's resolved type, plus the `HirId` of its declaring
/// `HirFunctionDef` -- unlike a field, a method has to be resolved back to a
/// callable symbol from *outside* the struct's own (already-popped)
/// analysis scope (see member-call resolution in `analysis.rs`), so its
/// declaration identity has to be recorded here, not just its type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedMethod {
    pub decl_id: HirId,
    pub fn_type: ResolvedFunctionType,
    /// `exposed`/`internal`/(default `Hidden`), resolved once here
    /// (alongside `annotations`) at signature time -- see
    /// `Analyzer::check_visibility`'s method-call hook (`resolve_callee`)
    /// for where this is actually enforced.
    pub visibility: Visibility,
    /// This method's own resolved `@inline`/`@mangling`/`@suppress` --
    /// resolved once, here, at signature time (never re-resolved at body-
    /// check time) so it's already known even for a method whose body this
    /// compilation never checks at all (an extern-owned method referenced
    /// via `--extern`) -- see `omega_driver::Driver::collect_extern_functions`,
    /// this field's only reader outside `check_struct_body`/`check_enum_body`/
    /// `check_union_body`.
    pub annotations: crate::annotations::ResolvedAnnotations,
}

/// A struct's fields and methods, shared behind `ResolvedType::Struct`'s
/// `Rc<RefCell<_>>` so that a self-referencing field (`next: *Node`, the
/// classic linked-list shape) can hold a live handle to the very type still
/// being built: the placeholder is inserted (with empty `fields`/
/// `functions`) *before* fields are resolved, and patched in place once
/// they're known -- every clone taken in the meantime (e.g. a pointer field
/// that pointed back to it) observes the same, eventually-complete data,
/// rather than a stale structural snapshot copied by value. Comparing two
/// `ResolvedType::Struct`s (see `PartialEq` below) never has to walk into
/// `fields`/`functions` at all, so this also sidesteps the infinite regress
/// a *structural* comparison of a self-referential type would otherwise be.
#[derive(Debug)]
pub struct ResolvedStructType {
    pub id: HirId,
    pub name: Ident,
    /// The absolute path of the module this struct is declared in --
    /// needed for the same reason `type_args` is (see its doc comment):
    /// a reference to this type from anywhere else only ever sees this
    /// cell, never the declaration site itself, and mangling a full
    /// symbol needs a full path, not just a bare name.
    pub module_path: Vec<Ident>,
    /// The concrete generic arguments this cell was instantiated with --
    /// empty for a non-generic struct. This is what lets a *reference* to
    /// this type from somewhere else (a field, a parameter, a return
    /// type -- anywhere that only ever sees this `Rc<RefCell<_>>` cell,
    /// never the original declaration site) still be mangled with its
    /// generic arguments intact; see `omega_driver`'s type cells,
    /// this field's only writer.
    pub type_args: Vec<ResolvedType>,
    /// `(name, type, visibility)` per field, in declaration order -- see
    /// `Analyzer::check_visibility`'s field-access hook
    /// (`resolve_field_projection`) for where the third element is enforced.
    pub fields: Vec<(Ident, ResolvedType, Visibility)>,
    pub functions: Vec<(Ident, ResolvedMethod)>,
    /// `@layout(...)`'s resolved `pack`/`align` -- `{1, 1}` (today's
    /// implicit, zero-padding layout) unless overridden. See
    /// `omega_codegen`'s layout functions (`total_bytes`/
    /// `field_byte_offset`), which are this field's only reader.
    pub layout: crate::annotations::Layout,
    /// `@suppress(...)`'s warning names -- resolved once here (alongside
    /// `layout`, by whatever first builds this cell) rather than
    /// re-resolved every time a method body is checked, so
    /// `Analyzer::check_struct_body` can just read it back without risking
    /// re-emitting the same annotation errors a second time.
    pub suppress: Vec<Ident>,
    /// Every spec this type's own `implements` clause *nominally* names,
    /// each already resolved to its cell + concrete type arguments (see
    /// `Analyzer::resolve_implements_clause`, this field's only writer) --
    /// deliberately **not** derivable from `functions` alone: a method
    /// merely *shaped* like a spec requirement (same name, same signature)
    /// is not the same fact as this type having actually declared `:
    /// Spec<Args>`, and nothing else records that distinction anywhere
    /// (`ResolvedMethod` carries no "which requirement (if any) this
    /// satisfies" provenance). `Analyzer::analyze_for_in` is this field's
    /// reader: real nominal `ToIterator<T>` conformance has to be checked
    /// against *this*, not against `type_implements_spec` (which is
    /// structural -- see its own doc comment -- and would happily accept a
    /// type that merely duck-types the right method shapes).
    pub implemented_specs: Vec<(Rc<RefCell<ResolvedSpecType>>, Vec<ResolvedType>)>,
    /// `true` for a `marker` declaration -- see
    /// `omega_parser::ast::statement::r#struct::StructStmt::is_marker`. The
    /// *only* thing this changes anywhere in the analyzer/codegen: it's
    /// what exempts this cell from the "a struct must have at least one
    /// sized field" check (`Analyzer::signature_of_struct`) -- everything
    /// else (implements-clause resolution, method dispatch, generics,
    /// dead-code tracking, spec/vtable coercion, layout) already works
    /// unmodified for a zero-field struct, which is deliberately why
    /// `marker` reuses this type wholesale instead of being a separate
    /// `ResolvedType` variant.
    pub is_marker: bool,
    /// `true` for a marker tagged `@glue` -- see
    /// `annotations::ResolvedAnnotations::glue`'s doc comment. Read back by
    /// the driver (alongside `implemented_specs`) to populate the
    /// whole-program gap/glue registry.
    pub is_glue: bool,
}

/// Nominal, not structural: two struct types are the same type iff they're
/// the same *declaration* (matching real language semantics -- two
/// unrelated structs that happen to share a field layout are still
/// different types), and, just as importantly, this never has to borrow
/// into (or recurse through) `fields`, which may reference this very struct
/// again -- comparing by id keeps that O(1) regardless.
impl PartialEq for ResolvedStructType {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl Eq for ResolvedStructType {}

/// Consistent with the identity-only `PartialEq` above -- hashing only
/// `id` (never `fields`/`functions`) is both correct (equal values must hash
/// equal, and equality here is id-only) and the only option that doesn't
/// recurse into a possibly self-referential struct's own fields.
impl Hash for ResolvedStructType {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

/// A union's fields and methods, shared behind `ResolvedType::Union`'s
/// `Rc<RefCell<_>>` for exactly the reasons `ResolvedStructType` is (see its
/// doc comment) -- same self-reference/placeholder-then-patch handling, same
/// nominal `PartialEq`/`Hash` below. The only real difference from a struct
/// is semantic (fields overlap in storage instead of being laid out
/// sequentially), which lives entirely in codegen/field-projection, not here.
#[derive(Debug)]
pub struct ResolvedUnionType {
    pub id: HirId,
    pub name: Ident,
    /// See `ResolvedStructType::module_path`'s doc comment.
    pub module_path: Vec<Ident>,
    /// See `ResolvedStructType::type_args`'s doc comment.
    pub type_args: Vec<ResolvedType>,
    /// See `ResolvedStructType::fields`'s doc comment.
    pub fields: Vec<(Ident, ResolvedType, Visibility)>,
    pub functions: Vec<(Ident, ResolvedMethod)>,
    /// See `ResolvedStructType::suppress`'s doc comment. Unions don't
    /// support `@layout` yet -- only `@suppress` applies here.
    pub suppress: Vec<Ident>,
    /// See `ResolvedStructType::implemented_specs`'s doc comment.
    pub implemented_specs: Vec<(Rc<RefCell<ResolvedSpecType>>, Vec<ResolvedType>)>,
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

/// An omega-style enum's fully resolved shape, shared behind
/// `ResolvedType::Enum`'s `Rc<RefCell<_>>` for exactly the reasons
/// `ResolvedStructType` is (see its doc comment): a variant body may point
/// back at the enum itself (`next: *MyEnum`), so the placeholder is
/// registered before variants are resolved and patched in place.
///
/// Everything a *use site* needs is here -- construction sites in any
/// module read the tag/header constants straight out of this cell, so the
/// per-variant constants only ever get analyzed once, at the definition.
#[derive(Debug)]
pub struct ResolvedEnumType {
    pub id: HirId,
    pub name: Ident,
    /// See `ResolvedStructType::module_path`'s doc comment.
    pub module_path: Vec<Ident>,
    /// See `ResolvedStructType::type_args`'s doc comment.
    pub type_args: Vec<ResolvedType>,
    /// Always an integer type -- `U16` for an implicit tag; whatever the
    /// header's leading `tag:` entry declared for an explicit one. Kept as
    /// a full `ResolvedType` (not a width/signedness pair) deliberately:
    /// the language intends to allow non-numeric tags eventually, and
    /// everything downstream already treats this as an opaque field type.
    pub tag_type: ResolvedType,
    /// The shared header fields, in declaration order -- *excluding* the
    /// tag, which is layout-wise field -1 (always first) and accessed via
    /// the dedicated `.tag` projection instead. See
    /// `ResolvedStructType::fields`'s doc comment for the 3-tuple shape.
    pub header: Vec<(Ident, ResolvedType, Visibility)>,
    /// The shared *dynamic* fields, in declaration order -- present on
    /// every variant like `header`, laid out right after it, but
    /// runtime-valued: every construction site supplies them (see
    /// `Analyzer::analyze_struct_literal`'s `EnumVariant` arm), and they're
    /// freely assignable afterward, exactly like a variant's own body
    /// field. Unlike `header`, there is no per-variant constant list here
    /// at all -- there's nothing to bake in.
    pub dynamic_fields: Vec<(Ident, ResolvedType, Visibility)>,
    pub variants: Vec<ResolvedEnumVariant>,
    /// Same shape and semantics as `ResolvedStructType::functions`.
    pub functions: Vec<(Ident, ResolvedMethod)>,
    /// Same shape and semantics as `ResolvedStructType::layout`, applied
    /// to the enum's own aggregate `[tag][header][dynamic][payload]`
    /// layout as a whole.
    pub layout: crate::annotations::Layout,
    /// See `ResolvedStructType::suppress`'s doc comment.
    pub suppress: Vec<Ident>,
    /// See `ResolvedStructType::implemented_specs`'s doc comment.
    pub implemented_specs: Vec<(Rc<RefCell<ResolvedSpecType>>, Vec<ResolvedType>)>,
}

/// One resolved variant: its unique tag value, its per-variant header
/// constants (one per `ResolvedEnumType::header` entry, positionally), and
/// its own body fields.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedEnumVariant {
    pub name: Ident,
    /// Compile-time constant, unique across the enum -- what the
    /// uniqueness check compared, and what construction emits at offset 0.
    pub tag: crate::checked::NumberValue,
    /// One constant per header field, positionally.
    pub header_values: Vec<ConstValue>,
    /// The variant-specific body fields -- empty for a body-less variant.
    /// At runtime the enum's body region is a union of all variants'
    /// bodies; analysis only ever lets the statically-known variant's own
    /// fields be touched. See `ResolvedStructType::fields`'s doc comment
    /// for the 3-tuple shape.
    pub fields: Vec<(Ident, ResolvedType, Visibility)>,
}

impl ResolvedEnumType {
    /// The variant named `name`, with its index -- the shape both variant
    /// construction and body-field lookup want.
    pub fn variant(&self, name: &Ident) -> Option<(usize, &ResolvedEnumVariant)> {
        self.variants.iter().enumerate().find(|(_, v)| &v.name == name)
    }
}

/// Nominal identity, exactly like `ResolvedStructType`'s -- see its
/// `PartialEq`/`Hash` doc comments; the same self-reference reasoning
/// applies (a variant body may embed `*MyEnum`).
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

/// A `spec`'s fully resolved shape, shared behind `ResolvedType::Spec`'s
/// `Rc<RefCell<_>>` for the same nominal-identity reasons `ResolvedStructType`
/// is (see its doc comment) -- though a spec is never self-referential the
/// way a struct field can be, so the placeholder-then-patch dance doesn't
/// apply here; it's still behind a cell purely so every reference to "this
/// spec" (an implements clause, a generic bound, a spec-object type) shares
/// one identity to compare/hash against, exactly like a struct reference
/// does.
///
/// `dependencies` are already resolved (not raw) -- a spec's own dependency
/// list needs no `Self`/generic substitution to know *which* specs it
/// requires (only the functions those specs require need substitution,
/// deferred to implementation time -- see `RawSpecFunctionSig`), so
/// resolving them eagerly here is what makes dependency-cycle detection
/// fall out of the ordinary `omega_driver::Driver::ensure_item`
/// `InProgress`/cycle machinery for free, with no spec-specific cycle guard
/// needed.
#[derive(Debug)]
pub struct ResolvedSpecType {
    pub id: HirId,
    pub name: Ident,
    /// `exposed`/`internal`/(default `Hidden`) -- the spec's own
    /// visibility, already checked wherever this spec is *named* (an
    /// ordinary item-visibility check, same as any other top-level item).
    /// Kept here too because every one of this spec's own functions
    /// *inherits* this same visibility (see `FlattenedSpecFn::visibility`'s
    /// doc comment) -- an implementor's own method satisfying one of them
    /// must be at least this permissive, checked in
    /// `Analyzer::resolve_implements_clause`.
    pub visibility: Visibility,
    pub generics: Vec<Ident>,
    /// See `ResolvedStructType::module_path`'s doc comment.
    pub module_path: Vec<Ident>,
    /// See `ResolvedStructType::type_args`'s doc comment -- the concrete
    /// arguments `generics` was substituted with, empty for a
    /// non-generic spec.
    pub type_args: Vec<ResolvedType>,
    /// Whether this spec can be used as a dynamic-dispatch trait object
    /// (`spec *Self`) at all -- `false` the instant any of `functions`
    /// declares a `spec T` (static-dispatch, no `*`) return type, directly
    /// or transitively through a dependency (a vtable slot can't point at
    /// "whichever concrete type each implementor happens to use" -- the
    /// exact reason Rust's `IntoIterator` isn't object-safe either).
    /// Computed once, eagerly, right where `functions`/`dependencies`
    /// themselves are first resolved (`omega_driver::Driver::
    /// resolve_spec_declaration`) -- a dependency's own cell is always
    /// already fully built by then, so this never needs its own
    /// resolution pass. Checked wherever a `Type::SpecObject` (`spec
    /// *Self`) actually resolves into a real `ResolvedType::SpecObject`
    /// value, so a not-object-safe spec is rejected at the one point that
    /// matters instead of scattered across every dynamic-dispatch call
    /// site.
    pub is_object_safe: bool,
    /// Each dependency's own cell, resolved eagerly (see `ModuleResolver::
    /// spec_declaration`), paired with its **raw**, unresolved type
    /// arguments -- deliberately not `Vec<ResolvedType>`: resolving them
    /// here, at this spec's own declaration, would need this spec's own
    /// generics already bound to something concrete, which they never are
    /// at this point (`spec Foo<T> : Bar<T>` -- `T` isn't concrete until a
    /// real implementor is known). Resolved lazily instead, in
    /// `Analyzer::flatten_spec_into`, once `Self` + this spec's own
    /// generics *are* bound -- the exact same deferral `functions` (below)
    /// already uses, for the identical reason.
    pub dependencies: Vec<(Rc<RefCell<ResolvedSpecType>>, Vec<Type>)>,
    pub functions: Vec<(Ident, RawSpecFunctionSig)>,
    /// Empty unless `is_gap` -- see `GapFunction`'s own doc comment for why
    /// a gap's functions get a real, eagerly resolved `ResolvedFunctionType`
    /// here, instead of staying deferred in `functions`/`RawSpecFunctionSig`
    /// like every ordinary spec's do. `docs/21-gaps-and-glue.md`'s call
    /// resolution (`GapPath::function(...)`) and codegen's synthesized
    /// extern declarations are this field's two readers.
    pub gap_functions: Vec<(Ident, GapFunction)>,
    /// The spec's own declaration span -- used to anchor
    /// `AnalysisWarningKind::UnfilledGap`, which has no single call site of
    /// its own to point at (see `docs/21-gaps-and-glue.md`). Not needed by
    /// anything else here, so it isn't threaded any further than that.
    pub span: Span,
    /// See `annotations::ResolvedAnnotations::gap`'s doc comment.
    pub is_gap: bool,
    /// This spec's own resolved `@suppress` list -- same shape and purpose
    /// as `ResolvedStructType::suppress`.
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

/// One function member of a spec, kept **raw** (unresolved `Type`s, an
/// unlowered `HirBlock` default body) -- directly mirroring
/// `omega_analyzer::resolver::GenericSignature`'s existing precedent for an
/// ordinary generic function's signature: a `Self`-referencing (or spec-
/// generic-referencing) type can't be resolved to a concrete
/// `ResolvedType` until a concrete implementor is known, so resolution is
/// deferred to that point (see `Analyzer::signature_of_struct`'s
/// implements-clause handling) rather than attempted here, at the spec's
/// own definition.
#[derive(Debug, Clone)]
pub struct RawSpecFunctionSig {
    pub decl_id: HirId,
    pub name: Ident,
    pub span: Span,
    /// See `ResolvedFunctionType::self_mode`. Always `Pointer`/`MutPointer`
    /// in practice -- by-value self is rejected at spec signature
    /// resolution (`Analyzer::resolve_spec_functions`).
    pub self_mode: Option<SelfMode>,
    /// Raw `HirParam`s (own id/span kept, per-param) rather than a plain
    /// `(Ident, Type)` list -- this is what lets a queued default-method
    /// instantiation reconstruct a real, ordinary `HirFunctionDef` later
    /// (see `Analyzer::check_pending_spec_method`) and reuse
    /// `check_function_body` wholesale, rather than duplicating its
    /// param-binding logic.
    pub params: Vec<HirParam>,
    pub return_type: Type,
    /// `None` for a required function (every implementor must provide its
    /// own matching method); `Some` for a default, used as-is unless a
    /// concrete implementor overrides it with its own same-named,
    /// same-signature method.
    pub default_body: Option<HirBlock>,
}

/// One `@gap` spec function, fully resolved -- unlike `RawSpecFunctionSig`,
/// which every *ordinary* spec function stays as (deferring type
/// resolution until a concrete implementor's `Self` is known). A gap
/// function is self-less by construction (`AnalysisErrorKind::
/// GapFunctionMustBeStatic`), so there's no `Self` to wait for -- its
/// signature is resolved exactly once, eagerly, right where the gap spec
/// itself is (`Analyzer::resolve_spec_functions`). A gap function with a
/// body is currently rejected outright (`AnalysisErrorKind::
/// GapFunctionBodyNotYetSupported`) -- see that error's own doc comment
/// for why default-bodied gap functions are deliberately out of scope for
/// now rather than half-supported.
#[derive(Debug, Clone)]
pub struct GapFunction {
    pub decl_id: HirId,
    pub span: Span,
    pub fn_type: ResolvedFunctionType,
}

/// A compile-time constant value -- what an enum variant's tag and header
/// values evaluate to at the definition, and what construction sites
/// re-emit. Covers exactly the primitive types a constant can currently be
/// written as; a header field whose type can't be represented here is
/// rejected at the enum's definition (see `AnalysisErrorKind::
/// EnumHeaderFieldUnsupportedType`), never at a use site.
#[derive(Debug, Clone, PartialEq)]
pub enum ConstValue {
    Number(crate::checked::NumberValue),
    Bool(bool),
    Char(char),
    /// A `*str` string constant -- the literal's decoded UTF-8 bytes.
    Str(String),
    /// A compile-time slice's elements (`&[...]`) -- no item type is
    /// carried here, exactly like `Str` doesn't carry its own type: it's
    /// always supplied externally by the enclosing `ResolvedType::Slice {
    /// item, .. }` at every call site (see `Analyzer::const_representable`,
    /// `Codegen::emit_const_value`). Codegen builds a separate rodata blob
    /// and stores a `[ptr, len]` fat pointer to it.
    Slice(Vec<ConstValue>),
    /// A fixed-length compile-time array's elements (a bare `[...]` against
    /// a `ResolvedType::SizedArray`-typed header field) -- unlike `Slice`,
    /// there's no indirection: codegen writes every element's leaves
    /// inline, back to back, directly into the enclosing storage (an enum's
    /// header region, or a nested array/slice element), exactly like an
    /// ordinary `SizedArray` value's own layout.
    Array(Vec<ConstValue>),
    /// A whole struct value, built by `comp` evaluation (see
    /// `crate::comp_eval`) -- fields in declared (`field_index`) order,
    /// mirroring `crate::checked::CheckedStructLiteral`'s own field-order
    /// guarantee, so codegen can write leaves in list order with no name
    /// lookup, exactly like `Array`'s elements already are.
    Struct(Vec<ConstValue>),
    /// A whole enum value, built by `comp` evaluation. `tag` and `header`
    /// are embedded directly (rather than re-derived from the enum's
    /// shared `ResolvedEnumType` cell via `variant_index` at every read)
    /// since a `ConstValue` carries no reference back to its own
    /// `ResolvedType` -- the one deliberate divergence from
    /// `CheckedEnumConstruct`'s shape, which *can* rely on the enclosing
    /// expression's own `r#type` instead. `header` is a straight clone of
    /// `ResolvedEnumVariant::header_values` (a per-variant *constant*, not
    /// per-instance data -- duplicated here purely so a `comp` evaluation
    /// can read `.header_field` back without needing type context, not
    /// because it's genuinely separate storage). `dynamic_fields`/`fields`
    /// split `CheckedEnumConstruct::fields`'s own combined "shared dynamic
    /// fields first, then this variant's own body fields" list back into
    /// its two real regions (see that type's doc comment) -- `comp_eval`'s
    /// own construction is what does the splitting.
    Enum {
        variant_index: usize,
        tag: crate::checked::NumberValue,
        header: Vec<ConstValue>,
        dynamic_fields: Vec<ConstValue>,
        fields: Vec<ConstValue>,
    },
    /// A whole union value, built by `comp` evaluation -- mirrors
    /// `CheckedUnionConstruct`: exactly one field written, at `field_index`.
    Union { field_index: usize, value: Box<ConstValue> },
    /// The address of another piece of `comp`-evaluated data (`&<place>`
    /// where `<place>` itself evaluated cleanly) -- generalizes what `Str`/
    /// `Slice` above already do ad hoc (both are secretly "pointer to a
    /// separately-built rodata blob") into one explicit indirection, so a
    /// struct field can point at e.g. a sibling `comp`-computed buffer, not
    /// just a string/slice literal. Never a pointer into *runtime* memory --
    /// the interpreter only ever produces one of these by evaluating a
    /// place it fully evaluated itself; see `comp_eval`'s rejection of any
    /// pointer the interpreter didn't itself produce.
    Ref(Box<ConstValue>),
}

/// How a numeric resolved type behaves arithmetically: its signedness (or
/// float-ness) and bit width. Shared by analysis (to validate a number
/// literal's suffix, range-check its value, and type-check `BinaryOp`/
/// `Negate` operands) and codegen (to pick the right instruction --
/// `sdiv`/`udiv`, `ineg`/`fneg`, ...) -- computed once here rather than
/// re-pattern-matched on `ResolvedType` at every call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumericKind {
    Signed(u32),
    Unsigned(u32),
    Float(u32),
}

/// A castable type's shape, for `<Target>expr` (see `ResolvedType::cast_class`):
/// its bit width, and (for the int family) signedness -- exactly what's
/// needed to pick a `CastKind` between any two castable types, purely from
/// their widths/signedness, with no per-type-pair table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CastClass {
    Int { width: u32, signed: bool },
    Float { width: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedType {
    Void,
    /// `never` -- a function's own declared return type meaning "this
    /// function does not return" (`exit(code: i32) => never`). Legal
    /// *only* in that one position (a function/method/extern/gap's own
    /// return type; `Context::resolve_type` rejects it everywhere else a
    /// type gets resolved for storage -- see `AnalysisErrorKind::
    /// NeverTypeNotAllowed`), and deliberately not threaded through
    /// `accepts`/general expression-type inference: it only ever needs to
    /// exist so `=> never` itself resolves, and so a call's looked-up
    /// return type can *be* `Never` at the one point that matters --
    /// `Analyzer::expr_diverges` recognizing such a call as diverging. From
    /// there, the existing `None`-means-diverges mechanism (`Analyzer::
    /// block_type`) already does everything a real coercing `!` type would
    /// -- see its own doc comment. No `never`-typed variables follow from
    /// this by construction: nothing ever needs to *store* a `Never` value,
    /// only recognize that a position produced one.
    Never,
    Bool,
    /// A single Unicode scalar value, stored as a 4-byte codepoint -- the
    /// same representation Rust's `char` uses (large enough to hold any
    /// UTF-8-encoded character, decoded). This is *not* what a C string's
    /// bytes are typed as; that's `U8` (see `*u8`'s use for `puts`/`printf`
    /// in `examples/dev/main.omg`) -- a byte and a decoded character are
    /// different things once `char` stops being an alias for "one byte".
    Char,
    I8,
    I16,
    I32,
    I64,
    /// Pointer-sized signed integer. Hardcoded to 64 bits in `numeric_kind`
    /// below, matching this compiler's existing single-target reality (see
    /// its doc comment) -- it tracks the *target's* pointer width, not a
    /// fixed alias for `i64`, unlike `into_ir_type`'s mapping of this variant
    /// to `codegen.pointer_type()`, which genuinely is target-correct.
    ISize,
    U8,
    U16,
    U32,
    U64,
    /// Pointer-sized unsigned integer. See `ISize`'s doc comment.
    USize,
    F32,
    F64,
    /// `*T` (`mutable: false`) or `*mut T` (`mutable: true`) -- whether the
    /// pointee may be written through (`Analyzer::analyze_place`'s running
    /// mutability, overwritten by every `Deref` it processes). Immutable by
    /// default, like every binding (`VarBinding::mutable`). This is a
    /// *type*-level fact, unrelated to whether the pointer *itself* (as a
    /// binding) can be reassigned to point elsewhere.
    Pointer { pointee: Box<ResolvedType>, mutable: bool },
    Function(ResolvedFunctionType),
    /// `*[]T` (`mutable: false`) or `*mut []T` (`mutable: true`) -- an
    /// unsized run of `T`: genuinely just a thin pointer value (one leaf,
    /// see `layout::leaves_of`) with array-like properties (indexing,
    /// slicing) -- the same C-decayed-array-parameter shape `argv : *[]*u8`
    /// in `examples/dev/main.omg` already uses, now a fully general,
    /// constructible type (see `Analyzer::array_pointer_cast_kind`) rather
    /// than only ever populated by the OS's own C entry-point convention.
    /// Mutability is a type-level fact exactly like `Pointer`'s own --
    /// whether `arr[i] = x` is legal follows this flag, never whatever
    /// binding holds the value (see `Analyzer::project_index`'s `Array`
    /// arm). This is *not* what `*[?]T` resolves to -- see `Slice` below,
    /// and `Context::resolve_pointer_type`'s dedicated production that
    /// produces it.
    Array(Box<ResolvedType>, bool),
    /// `[N]T` -- a sized, inline, contiguous run of exactly `N` `T`s.
    /// Unlike `Array`, this is a genuine value type: it's stored inline
    /// (locals, struct fields, ...) rather than referenced through a
    /// pointer, the same way a `Struct` is.
    SizedArray(Box<ResolvedType>, u32),
    /// `*[?]T` (`mutable: false`) or `*mut [?]T` (`mutable: true`) -- a fat
    /// pointer: a data pointer plus a length, unlike `Pointer` which is
    /// always a single thin pointer value. Never written as
    /// `Pointer(Array(_))`; see `Context::resolve_pointer_type`. `mutable`
    /// carries the same meaning `Pointer::mutable` does, for `slice[i] =
    /// value`.
    Slice { item: Box<ResolvedType>, mutable: bool },
    /// `*str` (`mutable: false`) or `*mut str` (`mutable: true`) -- a
    /// UTF-8 string slice: at runtime, the exact same fat-pointer shape
    /// `Slice { item: U8, .. }` has (`[data_ptr, len]`, no null
    /// terminator), but a genuinely distinct nominal type -- no implicit
    /// coercion to/from `Slice`/`Pointer` in either direction (see
    /// `accepts` below). No `item` field: always byte-shaped, so there's
    /// nothing to parameterize. `str` alone (unwrapped by `*`/`*mut`)
    /// names nothing -- it's deliberately never registered in
    /// `Context::new()`'s `defined_types`, so it only ever resolves to
    /// this variant via the raw-pointee special case in
    /// `Context::resolve_type`'s `Type::Pointer` arm; any other use falls
    /// through to the ordinary "unrecognized type name" diagnostic.
    Str { mutable: bool },
    Struct(Rc<RefCell<ResolvedStructType>>),
    /// A C/Rust-style union value -- see `ResolvedUnionType`'s doc comment.
    Union(Rc<RefCell<ResolvedUnionType>>),
    /// An omega-style enum value. `variant` is the *statically known*
    /// variant, when there is one: `MyEnum::Second { ... }` produces a
    /// value of type `MyEnum::Second` (variant `Some(1)`), and only such a
    /// refined value may touch that variant's own body fields; a plain
    /// `MyEnum` (variant `None` -- what every written-down type annotation
    /// resolves to) only exposes the tag and the shared header. A refined
    /// value is usable anywhere the plain enum is expected -- the one
    /// implicit widening this type system has; see `ResolvedType::accepts`.
    Enum {
        cell: Rc<RefCell<ResolvedEnumType>>,
        variant: Option<usize>,
    },
    /// A reference to a spec *definition* -- what an implements clause
    /// (`struct Dog : Animal`), a generic bound (`T: Animal`), or a
    /// spec-object type's pointee (`spec *Animal`) resolves the name
    /// `Animal` to. Never itself the type of a runtime value -- a `spec
    /// *Animal` *value*'s type is `SpecObject` below, not this.
    Spec(Rc<RefCell<ResolvedSpecType>>),
    /// `spec *Animal` (`mutable: false`) or `spec *mut Animal` (`mutable:
    /// true`) -- a dynamic-dispatch trait-object value: at runtime, a fat
    /// pointer (a data pointer plus a compiler-generated vtable pointer),
    /// exactly like `Slice`'s `[data_ptr, len]` shape is a fat pointer of a
    /// different kind (see `Codegen`'s `IntoIRType` impl, which flattens
    /// both to two leaves). The concrete pointee type is erased -- only
    /// that it implements `spec` (with these `type_args`, for a generic
    /// spec) is known.
    SpecObject {
        spec: Rc<RefCell<ResolvedSpecType>>,
        type_args: Vec<ResolvedType>,
        mutable: bool,
    },
}

/// Can't `#[derive(Hash)]` -- `Rc<RefCell<ResolvedStructType>>` isn't
/// `Hash` (std deliberately omits it for `RefCell`, since mutating a key
/// after it's hashed into a map would silently break the map's invariants).
/// Mirrors the manual `PartialEq` derived transitively through `Struct`
/// above: hash the borrowed cell's `id` only, never its `fields`/
/// `functions`, both for consistency with that equality and to avoid
/// recursing into a possibly self-referential struct.
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
            Self::SpecObject { spec, type_args, mutable } => {
                spec.borrow().hash(state);
                type_args.hash(state);
                mutable.hash(state);
            }
        }
    }
}

/// Renders the type exactly as a user would write it in Omega source
/// (`*u8`, `[i32; 3]`, `*[u8]`, `(s: *u8, ...) => i32`, a struct's bare
/// name) -- this is what every diagnostic shows, so it must read as the
/// language's own syntax, never as Rust debug output.
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
            Self::Pointer { pointee, mutable: false } => write!(f, "*{pointee}"),
            Self::Pointer { pointee, mutable: true } => write!(f, "*mut {pointee}"),
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
            Self::Array(inner, false) => write!(f, "*[]{inner}"),
            Self::Array(inner, true) => write!(f, "*mut []{inner}"),
            Self::SizedArray(inner, size) => write!(f, "[{size}]{inner}"),
            Self::Slice { item, mutable: false } => write!(f, "*[?]{item}"),
            Self::Slice { item, mutable: true } => write!(f, "*mut [?]{item}"),
            Self::Str { mutable: false } => write!(f, "*str"),
            Self::Str { mutable: true } => write!(f, "*mut str"),
            // Only the name, never the fields -- a struct may reference
            // itself, and its name is how source refers to it anyway.
            Self::Struct(cell) => write!(f, "{}", cell.borrow().name.as_ref()),
            Self::Union(cell) => write!(f, "{}", cell.borrow().name.as_ref()),
            // A refined enum type shows its known variant (`MyEnum::Second`)
            // -- that's exactly how source spells the construction that
            // produced it, and the refinement is load-bearing in the
            // diagnostics that mention it (body-field access rules).
            Self::Enum { cell, variant } => {
                let e = cell.borrow();
                write!(f, "{}", e.name.as_ref())?;
                if let Some(index) = variant {
                    write!(f, "::{}", e.variants[*index].name.as_ref())?;
                }
                Ok(())
            }
            Self::Spec(cell) => write!(f, "{}", cell.borrow().name.as_ref()),
            Self::SpecObject { spec, type_args, mutable } => {
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
    /// `Some` for exactly the types a number literal can resolve to, and the
    /// only types `BinaryOp`/`Negate`/`BitNot` operate on *directly*, with no
    /// conversion involved -- `Bool`/`Char`/pointers still get arithmetic and
    /// bitwise ops, just by first coercing to one of these (see
    /// `arithmetic_repr` below); `Bool` alone additionally gets a handful of
    /// ops natively, with no coercion at all (see `Analyzer::analyze_binary_op`).
    pub fn numeric_kind(&self) -> Option<NumericKind> {
        Some(match self {
            Self::I8 => NumericKind::Signed(8),
            Self::I16 => NumericKind::Signed(16),
            Self::I32 => NumericKind::Signed(32),
            Self::I64 => NumericKind::Signed(64),
            // Hardcoded to 64 bits -- see `ISize`/`USize`'s doc comments.
            Self::ISize => NumericKind::Signed(64),
            Self::U8 => NumericKind::Unsigned(8),
            Self::U16 => NumericKind::Unsigned(16),
            Self::U32 => NumericKind::Unsigned(32),
            Self::U64 => NumericKind::Unsigned(64),
            Self::USize => NumericKind::Unsigned(64),
            Self::F32 => NumericKind::Float(32),
            Self::F64 => NumericKind::Float(64),
            _ => return None,
        })
    }

    /// The numeric type a non-numeric operand implicitly *coerces* to for an
    /// arithmetic or bitwise op (`+ - * / % & | ^ << >>` binary, `~` unary) --
    /// `None` for anything with no such stand-in, including `Bool` (see
    /// `Analyzer::analyze_binary_op`'s doc comment for why `Bool` is handled
    /// natively instead of through this) and everything else that simply
    /// isn't arithmetic-eligible at all (structs, functions, ...).
    ///
    /// The coerced value is always this returned type, *never* cast back to
    /// `self` implicitly -- `some_char + 1` is a `u32`, not a `char`, and
    /// `some_char += 1` still doesn't type-check (the result would need an
    /// explicit cast back to assign into a `char` place). This is what keeps
    /// `Char`'s coercion sound despite `Char` having no validating
    /// constructor yet (see its own doc comment): there is no path back into
    /// `Char` from arbitrary arithmetic, only ever further arithmetic on a
    /// plain, unconstrained `u32`.
    ///
    /// The chosen representative always matches the exact scalar
    /// `layout::Leaf` codegen already stores the type as (`Char` as
    /// `Leaf::I32`, a pointer as `Leaf::Ptr`, both target-pointer-width) --
    /// so the coercion is always a same-width `CastKind::Reinterpret`, free
    /// at runtime, purely a compile-time relabeling.
    pub fn arithmetic_repr(&self) -> Option<ResolvedType> {
        match self {
            Self::Char => Some(ResolvedType::U32),
            Self::Pointer { .. } => Some(ResolvedType::USize),
            _ => None,
        }
    }

    /// This type's byte size, for a `sizeof<...>` used *inside* an
    /// annotation argument (`@layout(pack = sizeof<usize>)`) -- deliberately
    /// scoped to primitives only (`None` for structs/enums/unions/arrays/
    /// slices/functions/spec objects): a primitive's size needs no real
    /// backend to know, only the same hardcoded-64-bit-pointer convention
    /// `numeric_kind`/`cast_class` already use (see their doc comments), so
    /// `@layout`'s arguments can be resolved eagerly, in the analyzer, with
    /// the same span-anchored `Diagnostic` quality a plain integer literal
    /// gets. `sizeof<Type>` used as an ordinary *expression* (see
    /// `CheckedExpr::Sizeof`) is not scoped this way -- it supports any
    /// type, computed in codegen via the already-general `total_bytes`.
    pub fn primitive_byte_size(&self) -> Option<u32> {
        match self {
            Self::Bool => Some(1),
            Self::Char => Some(4),
            // Hardcoded 64-bit -- see `cast_class`'s identical precedent for
            // treating a pointer as a 64-bit integer.
            Self::Pointer { .. } => Some(8),
            _ => self.numeric_kind().map(|kind| {
                let width = match kind {
                    NumericKind::Signed(w) | NumericKind::Unsigned(w) | NumericKind::Float(w) => w,
                };
                width / 8
            }),
        }
    }

    /// This type's shape for `<Target>expr` casting purposes -- `None` for
    /// anything a cast can't touch at all (structs/enums/unions/slices/
    /// `void`/functions; see `AnalysisErrorKind::InvalidCast`). A pointer
    /// counts as an unsigned 64-bit int -- this compiler's existing
    /// single-target assumption (exactly matching `numeric_kind`'s own
    /// hardcoded 64-bit `isize`/`usize` above), and literally true at the IR
    /// level: `Codegen::pointer_type()` already returns the same Cranelift
    /// type an ordinary 64-bit integer would. This one case is what makes
    /// pointer<->pointer, pointer<->integer, and integer<->pointer casts all
    /// fall out of the *same* int-to-int width rules `Analyzer::
    /// resolve_cast_kind` applies, with no special-casing beyond it.
    ///
    /// `Char`/`Bool` get a class the same way (their own scalar
    /// representation's width -- 32 and 8 bits respectively, both
    /// unsigned), but **only ever as the source** of a cast: `resolve_
    /// cast_kind` has no notion of direction, so on its own this would
    /// symmetrically allow casting arbitrary integers *into* `Char`/`Bool`
    /// too, which isn't sound (not every `u32` is a valid codepoint, and
    /// there's no implicit "nonzero is true"). `Analyzer::analyze_cast`
    /// gates that asymmetry explicitly (see `allows_cast_into`) rather than
    /// this method trying to encode a direction it has no way to express.
    pub fn cast_class(&self) -> Option<CastClass> {
        if let Some(kind) = self.numeric_kind() {
            return Some(match kind {
                NumericKind::Signed(width) => CastClass::Int { width, signed: true },
                NumericKind::Unsigned(width) => CastClass::Int { width, signed: false },
                NumericKind::Float(width) => CastClass::Float { width },
            });
        }
        match self {
            Self::Pointer { .. } => Some(CastClass::Int { width: 64, signed: false }),
            Self::Char => Some(CastClass::Int { width: 32, signed: false }),
            Self::Bool => Some(CastClass::Int { width: 8, signed: false }),
            _ => None,
        }
    }

    /// The inclusive `[min, max]` domain of every representable value of
    /// this type, as `i128` (comfortably spans every integer type from
    /// `i8` to `u64`/`usize`, plus `bool`'s `{0,1}`) -- what a `match`'s
    /// interval-exhaustiveness check (`crate::exhaustiveness`) treats as
    /// "the whole domain" a numeric/`bool`/`char` match must cover. `None`
    /// for every other type: `match` support is deliberately scoped to
    /// enums, integers, `bool`, and `char` for now (see
    /// `AnalysisErrorKind::UnsupportedMatchScrutinee`).
    ///
    /// `Char`'s domain is `0..=0x10FFFF` (`char::MAX`), the full range of a
    /// Unicode scalar value -- it does *not* carve out the surrogate hole
    /// (`0xD800..=0xDFFF`), which is fine, not unsound: a real `char` value
    /// can never actually land in that hole in the first place (char
    /// literals are validated through `char::from_u32` at parse time), so
    /// this interval abstraction just doesn't know about a gap nothing can
    /// ever fall into. A match covering the full `0..=0x10FFFF` range is
    /// correctly recognized as exhaustive; it just can't (today) recognize
    /// a match that covers the domain *around* the hole without touching
    /// it as exhaustive without an `else` -- a minor conservatism, not a
    /// correctness gap.
    pub fn integer_domain(&self) -> Option<(i128, i128)> {
        Some(match self {
            Self::Bool => (0, 1),
            Self::Char => (0, char::MAX as i128),
            Self::I8 => (i8::MIN as i128, i8::MAX as i128),
            Self::I16 => (i16::MIN as i128, i16::MAX as i128),
            Self::I32 => (i32::MIN as i128, i32::MAX as i128),
            // `ISize` is hardcoded to 64 bits -- see `numeric_kind`'s doc
            // comment.
            Self::I64 | Self::ISize => (i64::MIN as i128, i64::MAX as i128),
            Self::U8 => (u8::MIN as i128, u8::MAX as i128),
            Self::U16 => (u16::MIN as i128, u16::MAX as i128),
            Self::U32 => (u32::MIN as i128, u32::MAX as i128),
            Self::U64 | Self::USize => (u64::MIN as i128, u64::MAX as i128),
            _ => return None,
        })
    }

    /// The same type with any statically-known enum-variant refinement
    /// erased (`MyEnum::Second` -> `MyEnum`) -- what inference positions
    /// that must stay variant-agnostic (an `if`'s unified branch type, an
    /// array literal's element type, a deduced generic argument) normalize
    /// to. Shallow on purpose: refinement only ever exists at the top level
    /// of a value's type (nothing written down in source can nest one).
    pub fn widened(&self) -> ResolvedType {
        match self {
            Self::Enum { cell, variant: Some(_) } => Self::Enum { cell: cell.clone(), variant: None },
            other => other.clone(),
        }
    }

    /// Whether a value of type `value` can be supplied where `self` is
    /// expected: exact equality, plus the one implicit widening this type
    /// system has -- a variant-refined enum value (`MyEnum::Second`) is
    /// usable as its plain enum (`MyEnum`). Never the reverse (a plain
    /// value's variant isn't known).
    ///
    /// This widening also applies through exactly one level of *immutable*
    /// pointer/slice indirection (`*MyEnum::Second` usable as `*MyEnum`) --
    /// sound specifically because of which pointers are ever allowed to
    /// carry a refined pointee in the first place: `&value` only keeps a
    /// refinement when it's a *permanent* fact about `value`'s own
    /// declared/inferred type (see `VarBinding::narrowed` and
    /// `Analyzer`'s `HirExpr::AddressOf` arm), and a permanently-refined
    /// binding can never be reassigned a different variant (this same
    /// `accepts` rule, applied at every assignment, already rejects that).
    ///
    /// Deliberately **not** extended to mutable pointers/slices at all --
    /// `*mut MyEnum::Second` never widens to `*mut MyEnum`, full stop, even
    /// though the exact same reasoning above would make it locally sound at
    /// this one call site. The reason is what happens *after*: a widened
    /// `*mut MyEnum` handed to unconstrained code could be used to write a
    /// *different* variant through it, silently invalidating whatever
    /// *other* binding/pointer still believes the underlying storage is
    /// `MyEnum::Second` (the original aliasing hole this whole mutability
    /// system exists to close). `&mut place`/`mut self`'s auto-ref close
    /// this at the *source* instead (see `Analyzer`'s `HirExpr::AddressOf`
    /// arm and `Context::widen_variable`): they always produce an already-
    /// widened mutable pointer and immediately widen the source binding's
    /// own tracked type too, so a refined mutable pointer only ever exists
    /// as a `match`-narrowed *view* of an already-mutable place, never as
    /// something `accepts` needs to reason about widening further.
    ///
    /// A mutable pointer/slice *does* freely coerce into an immutable one
    /// of the same (or widening-compatible) pointee, symmetric with a
    /// mutable binding being just as readable as an immutable one --
    /// captured below by `mutable: false` on `self`'s side alone.
    pub fn accepts(&self, value: &ResolvedType) -> bool {
        if self == value {
            return true;
        }
        match (self, value) {
            (Self::Enum { cell: expected, variant: None }, Self::Enum { cell: found, variant: Some(_) }) => {
                expected.borrow().id == found.borrow().id
            }
            (Self::Pointer { pointee: expected, mutable: false }, Self::Pointer { pointee: found, .. }) => {
                expected.accepts(found)
            }
            (Self::Slice { item: expected, mutable: false }, Self::Slice { item: found, .. }) => {
                expected.accepts(found)
            }
            (Self::Array(expected, false), Self::Array(found, _)) => expected.accepts(found),
            // No `item` to recurse on, unlike `Slice` above -- and
            // deliberately its own arm, not folded into `Slice`'s: `*str`
            // and `*[u8]` must never accept one another implicitly (see
            // `Str`'s own doc comment), only `*mut str` -> `*str` widening.
            (Self::Str { mutable: false }, Self::Str { .. }) => true,
            _ => false,
        }
    }

    /// The `mutable` flag of any pointer-shaped type (`Pointer`/`Slice`/
    /// `Str`/`Array`) -- `None` for anything else. Lets a single check
    /// (e.g. "a cast can't turn an immutable pointer-shaped value into a
    /// mutable one") apply uniformly across all four instead of being
    /// duplicated per shape.
    /// The module and declaration this type's own members belong to -- what
    /// a member-visibility check needs. `None` for anything with no
    /// declaration of its own (a primitive, a slice, a pointer): the only
    /// members those ever have come from a `for`-attached spec, which are
    /// always `Exposed` and so never need one.
    pub fn declaring_owner(&self) -> Option<(Vec<Ident>, HirId)> {
        match self {
            Self::Struct(cell) => Some((cell.borrow().module_path.clone(), cell.borrow().id)),
            Self::Union(cell) => Some((cell.borrow().module_path.clone(), cell.borrow().id)),
            Self::Enum { cell, .. } => Some((cell.borrow().module_path.clone(), cell.borrow().id)),
            _ => None,
        }
    }

    /// This type with at most one pointer hop removed -- the seamless
    /// autoderef every member lookup applies (`ptr.field`, `ptr.method()`),
    /// and never more than one level: `**Struct` still needs an explicit
    /// deref of its own.
    pub fn autoderef(&self) -> &ResolvedType {
        match self {
            Self::Pointer { pointee, .. } => pointee,
            other => other,
        }
    }

    pub fn pointer_like_mutable(&self) -> Option<bool> {
        match self {
            Self::Pointer { mutable, .. } | Self::Slice { mutable, .. } | Self::Str { mutable } => Some(*mutable),
            Self::Array(_, mutable) => Some(*mutable),
            _ => None,
        }
    }
}
