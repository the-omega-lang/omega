use cranelift::{
    codegen::{
        self,
        ir::{ArgumentPurpose, FuncRef, StackSlot},
    },
    prelude::{
        AbiParam, Block, Configurable, FloatCC, FunctionBuilder, FunctionBuilderContext, InstBuilder,
        IntCC, MemFlags, Signature, StackSlotData, StackSlotKind, TrapCode, Type as IRType, Value,
        isa, settings, types,
    },
};
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module};
use cranelift_object::{ObjectBuilder, ObjectModule};
use omega_analyzer::{
    annotations::ManglingMode,
    checked::{CastKind, ExternFunctionKind, ExternFunctionRef, NumberValue},
    resolved_type::{
        ConstValue, NumericKind, ResolvedEnumType, ResolvedFunctionType, ResolvedSpecType, ResolvedStructType,
        ResolvedType, ResolvedUnionType,
    },
};
use omega_hir::{BinaryOp, HirId};
use omega_mir::{
    MirAddressOf, MirArrayLiteral, MirAssignment, MirBinaryOp, MirCast, MirDynamicCall, MirEnumConstruct, MirExpr,
    MirExprNode, MirExternDeclaration, MirFunctionCall, MirFunctionDef, MirItem, MirModule, MirPlace, MirPlaceRoot,
    MirProjection, MirSlice, MirSpecCoerce, MirStructLiteral, MirTerminator, MirUnionConstruct,
};
use omega_parser::prelude::Ident;
use std::{cell::RefCell, collections::HashMap, rc::Rc, sync::Arc};

mod mangle;
mod target;
pub use target::{Arch, Os, Target, TargetParseError};

/// How aggressively Cranelift optimizes the generated code -- `-O<n>` maps
/// onto this. Cranelift's own `opt_level` setting only has three values
/// (`none`/`speed`/`speed_and_size`), one fewer than the four levels this
/// enum offers, so `O1`/`O2` deliberately collapse onto the same Cranelift
/// setting (see `cranelift_setting`) rather than inventing a distinction
/// Cranelift itself doesn't make.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OptLevel {
    #[default]
    O0,
    O1,
    O2,
    O3,
}

impl OptLevel {
    fn cranelift_setting(self) -> &'static str {
        match self {
            OptLevel::O0 => "none",
            OptLevel::O1 | OptLevel::O2 => "speed",
            OptLevel::O3 => "speed_and_size",
        }
    }
}

/// What `Codegen::finish` should produce -- see its own doc comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EmitKind {
    #[default]
    Obj,
    /// Cranelift's own CLIF text for every function -- backend-dependent by
    /// nature (a future non-Cranelift backend would have its own IR, or
    /// none at all); always available here at zero cost, since
    /// `cranelift_codegen::ir::Function` already implements `Display`.
    Ir,
    /// Cranelift's own per-target instruction listing for every function
    /// (`Context::set_disasm`/`CompiledCode.disasm`) -- real per-target
    /// assembly mnemonics, but generated during codegen rather than by
    /// disassembling the final linked object's bytes (that would need
    /// Cranelift's `disas` feature, which pulls in `capstone`, a C
    /// library -- deliberately not taken on for this).
    Asm,
}

/// `Codegen::finish`'s result -- an object file's bytes for `EmitKind::Obj`,
/// or human-readable text (CLIF/assembly, one section per function) for
/// `Ir`/`Asm`. The caller (`omgc`) writes either straight to the output
/// path via `std::fs::write`, which accepts both.
pub enum EmitOutput {
    Object(Vec<u8>),
    Text(String),
}

/// Codegen never fails on the *program* itself: everything it would
/// otherwise need to reject was already enforced while building the
/// `CheckedModule` these `MirModule`s were lowered from (place validity,
/// type compatibility, field/index existence, redeclaration). What remains
/// here are cases the language genuinely hasn't decided yet (array memory
/// layout, global data storage, ...) -- those `panic!`/`todo!()` rather than
/// returning an error, since there is no rejectable *program* input left by
/// the time codegen runs, only unimplemented compiler features. The one
/// exception is `generate` itself: a `--target`/ISA construction failure is
/// genuinely rejectable *CLI* input (unlike anything about the program
/// being compiled), so it alone returns a `Result`.
pub struct Codegen {
    // Backend
    isa: Arc<dyn isa::TargetIsa>,
    pub module: ObjectModule,
    functions: HashMap<HirId, FuncId>,
    ctx: codegen::Context,
    emit: EmitKind,
    /// Accumulated by `define_function_def` when `emit` is `Ir`/`Asm` --
    /// empty (and never appended to) for `Obj`, so the common case pays no
    /// cost for a feature it isn't using.
    captured_text: String,

    // Global state
    /// Every anonymous byte-run constant this module has emitted so far --
    /// `"..."` (`*str`) and `b"..."` (`*[u8]`) literals alike, deduplicated
    /// by raw content in one map: neither is null-terminated, so identical
    /// text used once each way produces byte-for-byte identical storage,
    /// and sharing one `DataId` between them is exactly right (they only
    /// ever differ in the *type* the surrounding expression carries, never
    /// in the bytes themselves).
    bytes: HashMap<String, DataId>,
    /// One vtable per `(concrete type, spec)` pair actually coerced to a
    /// `spec *Spec` value somewhere in this compilation -- built lazily,
    /// the first time a `MirExpr::SpecCoerce` for that exact pair is
    /// codegen'd (see `vtable_for`), and shared by every later coercion of
    /// the same pair. Keyed by each side's own `HirId` rather than the
    /// full `ResolvedType`/`Rc<RefCell<_>>` -- cheap, `Eq`/`Hash`, and
    /// already the identity both `ResolvedType`'s own manual `PartialEq`/
    /// `Hash` and `self.functions` use.
    vtables: HashMap<(HirId, HirId), DataId>,

    // Local state (must be cleared per function)
    /// `bytes`'s per-function counterpart -- caches just the data pointer
    /// (the length, when needed, is a cheap `iconst` recomputed each call,
    /// same as any other compile-time-constant length).
    local_bytes: HashMap<String, Value>,
    /// One entry per `MirBody::locals` index -- `local_args[i]` is
    /// non-empty exactly when local `i` is a parameter (`i < arg_count`),
    /// caching its already-materialized SSA leaves straight from the entry
    /// block's own Cranelift params (never a stack slot, unless something
    /// later takes its address -- see `place_storage_address`'s own
    /// `todo!()`).
    local_args: Vec<Vec<Value>>,
    /// `MirBody::locals`-indexed, one stack slot per *non-parameter* local
    /// (`i >= arg_count`), sized to its type's total byte size (not one
    /// slot per scalar leaf) -- a prerequisite for `&`/`*`: a local needs a
    /// single address, and three independent per-leaf slots have three
    /// unrelated addresses. Field access within it is a byte offset (see
    /// `total_bytes`/`field_byte_offset`), not a leaf-count slice. Sized to
    /// `MirBody::locals.len()` up front (all `None`) but each entry is only
    /// actually allocated lazily, the first time `resolve_place_storage`
    /// resolves that local -- a branch that never runs never pays for a
    /// slot it never touches, and (unlike allocating every slot up front
    /// regardless of reachability) this is what `process_decl` already did
    /// before the mir existed, just now triggered by first-use instead of
    /// by walking a `Declaration` statement's own lexical position.
    stack_slots: Vec<Option<StackSlot>>,
    /// The current function's own `MirBody::arg_count` -- the boundary
    /// `local_args`/`stack_slots` use to tell a parameter local apart from
    /// a declared/synthetic one (see `MirBody::locals`'s own doc comment
    /// for why both share one index space).
    arg_count: usize,

    /// Every locally-defined function/method's final (post-mangling-or-not)
    /// linker symbol, keyed by the demangled string actually handed to
    /// `cranelift_module::declare_function` -- built up as
    /// `declare_function_def` runs for every item across every module (see
    /// `update_all`). A second, different function claiming a symbol
    /// already seen (only possible via `@mangling(disabled)`, since a
    /// mangled name always embeds a unique module path + `HirId`) is caught
    /// here instead of surfacing as a confusing linker error or, worse, a
    /// silent single-definition merge.
    declared_symbols: HashMap<String, HirId>,
    /// The first symbol collision found (see `declared_symbols`) -- checked
    /// once, at the end of `Codegen::generate`, and turned into that
    /// function's `Err`.
    symbol_error: Option<String>,
}

/// Where a resolved place's underlying storage lives, for both the read
/// (producing values) and write (storing values) case:
enum PlaceStorage {
    /// Already-materialized SSA values (a parameter local that hasn't been
    /// dereferenced through) -- readable, but has no address: there is no
    /// memory location backing a bare SSA value.
    Values(Vec<Value>),
    /// A byte offset into one compile-time-known stack slot (a non-
    /// parameter local, before any `Deref`).
    Slot { slot: StackSlot, offset: u32 },
    /// A byte offset from a runtime pointer value -- the state from the
    /// first `Deref` projection onward (explicit `*`, or a seamless
    /// pointer-to-struct field access), since the pointee isn't known until
    /// runtime.
    Address { base: Value, offset: u32 },
}

trait IntoIRType {
    fn into_ir_type(self, codegen: &Codegen) -> Vec<IRType>;
}

impl IntoIRType for ResolvedType {
    fn into_ir_type(self, codegen: &Codegen) -> Vec<IRType> {
        match self {
            ResolvedType::Void => vec![],
            // `Bool` is a plain 0/1 byte -- cranelift's integer types are
            // sign-agnostic and there's no dedicated boolean IR type to use
            // instead (see `ResolvedType::Bool`'s doc comment).
            ResolvedType::Bool => vec![types::I8],
            // A decoded 4-byte Unicode scalar value, not a byte -- see
            // `ResolvedType::Char`'s doc comment for why this isn't `I8`.
            ResolvedType::Char => vec![types::I32],
            ResolvedType::I8 | ResolvedType::U8 => vec![types::I8],
            ResolvedType::I16 | ResolvedType::U16 => vec![types::I16],
            ResolvedType::I32 | ResolvedType::U32 => vec![types::I32],
            ResolvedType::I64 | ResolvedType::U64 => vec![types::I64],
            // Unlike every other numeric variant, this one's IR type is
            // genuinely target-dependent -- `into_ir_type` already takes
            // `&Codegen`, so this is the one place `usize`/`isize` actually
            // track the real pointer width rather than a hardcoded 64 bits
            // (see `ResolvedType::USize`/`ISize`'s doc comments).
            ResolvedType::USize | ResolvedType::ISize => vec![codegen.pointer_type()],
            ResolvedType::F32 => vec![types::F32],
            ResolvedType::F64 => vec![types::F64],
            // Interior gaps (from a field's own transitive `align`, or from
            // this struct's own `@layout(pack = n)` chunking -- see
            // `place_field`) and any trailing padding this struct's own
            // `@layout(align = n)` demands are real filler `I8` leaves here,
            // not just a byte-offset bookkeeping detail: this leaf list is
            // also what a parameter struct value *is* (flattened positional
            // scalars, see this file's module doc), so the gaps have to
            // actually exist as leaves for `field_byte_offset`'s
            // memory-side byte offsets and this leaf-list's own positions to
            // keep agreeing with each other.
            ResolvedType::Struct(struct_type) => {
                let struct_type = struct_type.borrow();
                let field_types: Vec<ResolvedType> = struct_type.fields.iter().map(|(_, t, _)| t.clone()).collect();
                let layout = layout_fields(&field_types, struct_type.layout.pack, codegen);
                let mut leaves = layout.leaves;
                let final_size = round_up(layout.packed_end, struct_type.layout.align);
                leaves.extend(std::iter::repeat_n(types::I8, (final_size - layout.packed_end) as usize));
                leaves
            }
            // Every field overlaps the same storage -- exactly the shape a
            // single enum variant's payload has (see `enum_payload_bytes`'s
            // doc comment), so this reuses the same opaque-chunk flattening,
            // with no tag/header leaves in front of it. Unions don't support
            // `@layout` (see `ResolvedUnionType::suppress`'s doc comment),
            // so there's no alignment/padding concern here at all.
            ResolvedType::Union(union_type) => {
                payload_chunks(union_bytes(&union_type.borrow(), codegen))
            }
            // An enum value is `[tag][header fields][shared dynamic fields]
            // [payload]` -- the tag, header, and shared dynamic fields all
            // flatten like ordinary struct fields (the dynamic fields are
            // simply ordinary per-instance storage, unlike the header's
            // per-variant constants), while the payload (a union of every
            // variant's body, sized to the largest) flattens to opaque
            // integer chunks: no single typed leaf list can describe a
            // union, so the chunks only ever move bytes around (assignment,
            // parameter passing); a body field is read/written through
            // memory at its byte offset instead (see `resolve_place_
            // storage`'s `EnumBody` arm). A statically-known variant
            // refinement never changes the layout -- every enum value is
            // full-size, which is exactly what makes refined -> plain
            // widening a plain leaf copy. Interior/trailing padding is
            // handled exactly like `Struct`'s arm above -- see its doc
            // comment; the payload's own start additionally respects the
            // largest alignment any variant's own body field demands (see
            // `enum_payload_alignment`), since every variant shares that one
            // starting offset.
            ResolvedType::Enum { cell, .. } => {
                let enum_type = cell.borrow();
                let prefix = enum_prefix_layout(&enum_type, codegen);
                let mut leaves = prefix.leaves;

                let payload_align = enum_payload_alignment(&enum_type);
                let payload_size = enum_payload_bytes(&enum_type, enum_type.layout.pack, codegen);
                let payload_offset = place_field(prefix.packed_end, payload_align, payload_size, enum_type.layout.pack);
                leaves.extend(std::iter::repeat_n(types::I8, (payload_offset - prefix.packed_end) as usize));
                leaves.extend(payload_chunks(payload_size));

                let final_size = round_up(payload_offset + payload_size, enum_type.layout.align);
                leaves.extend(std::iter::repeat_n(
                    types::I8,
                    (final_size - (payload_offset + payload_size)) as usize,
                ));
                leaves
            }
            // `N` copies of the item type's own leaves, back to back -- the
            // same packed, no-padding layout a `Struct`'s fields get.
            ResolvedType::SizedArray(item_type, size) => {
                let item_leaves = item_type.into_ir_type(codegen);
                std::iter::repeat_n(item_leaves, size as usize).flatten().collect()
            }
            // A fat pointer: a data pointer plus an `i32` length. See
            // `ResolvedType::Slice`'s doc comment for why this is a distinct
            // variant rather than `Pointer(Array(_))`. `Str` shares the
            // identical leaf shape (see its own doc comment) but is kept a
            // separate arm rather than folded into this one, matching how
            // it's a fully separate `ResolvedType` variant, not a
            // structural alias.
            ResolvedType::Slice { .. } | ResolvedType::Str { .. } => vec![codegen.pointer_type(), types::I32],
            // `Pointer`, `Function`, and the legacy unsized `Array` (see its
            // doc comment) are all a single thin pointer value.
            ResolvedType::Pointer { .. } | ResolvedType::Function(_) | ResolvedType::Array(_) => {
                vec![codegen.pointer_type()]
            }
            // `Spec` is a reference to a spec *definition*, never a runtime
            // value of its own -- it never actually reaches codegen (only
            // `SpecObject`, an actual value type, does); no leaves make
            // sense for it.
            ResolvedType::Spec(_) => unreachable!("a spec definition is never itself a value type"),
            // `spec *Animal`: a fat pointer, exactly like `Slice`'s
            // `[data_ptr, len]` shape above -- a data pointer plus a
            // compiler-generated vtable pointer, both plain thin pointers.
            ResolvedType::SpecObject { .. } => vec![codegen.pointer_type(), codegen.pointer_type()],
        }
    }
}

/// Slices a `FieldAccess` projection's already-resolved `field_index` out of
/// an already-materialized value list (a `PlaceStorage::Values`, i.e. a
/// parameter local that hasn't been dereferenced through -- positional, by
/// leaf count, since there's no memory/byte offset for a bare SSA value).
/// No name search, no failure path: the mir already picked this exact index
/// out of `struct_type`.
fn project_field_access<T: Clone>(
    codegen: &Codegen,
    values: &[T],
    struct_type: &ResolvedStructType,
    field_index: usize,
) -> Vec<T> {
    let field_types: Vec<ResolvedType> = struct_type.fields.iter().map(|(_, t, _)| t.clone()).collect();
    let start = layout_fields(&field_types, struct_type.layout.pack, codegen).leaf_starts[field_index];
    let len = struct_type.fields[field_index].1.clone().into_ir_type(codegen).len();

    values[start..start + len].to_vec()
}

/// A resolved type's total in-memory size, in bytes: the sum of its scalar
/// leaves' sizes (`into_ir_type` already flattens a struct/enum recursively
/// into its leaves -- interior/trailing padding included, see their
/// `into_ir_type` arms -- so this needs no separate struct/enum case of its
/// own). Layout is packed by default -- each field sits at the raw running
/// byte sum of its predecessors -- unless `@layout(pack = ...)`/`@layout(
/// align = ...)` says otherwise somewhere in the type graph (see
/// `type_alignment`/`place_field`); x86_64 tolerates unaligned loads/stores
/// with no correctness issue, so packed is safe as a default, it's just not
/// C-ABI-compatible layout, consistent with the rest of this codegen not
/// implementing true C-ABI struct-passing conventions at function
/// boundaries either (structs are passed as flattened positional scalars,
/// not per SysV aggregate rules).
fn total_bytes(r#type: ResolvedType, codegen: &Codegen) -> u32 {
    r#type.into_ir_type(codegen).iter().map(|t| t.bytes()).sum()
}

/// A struct/enum's own alignment requirement when embedded as a field --
/// `1` (no alignment; today's implicit default) for everything except an
/// explicit `@layout(align = n)` struct/enum, which imposes `n`. The *only*
/// source of alignment anywhere in this layout model: never inferred from a
/// primitive's own natural width, so a struct/enum with no `@layout(align =
/// ...)` anywhere in its own or its fields' types keeps today's
/// byte-identical fully packed layout. Unrelated to `pack` -- see `Layout`'s
/// own doc comment for why the two are orthogonal.
fn type_alignment(r#type: &ResolvedType) -> u32 {
    match r#type {
        ResolvedType::Struct(cell) => cell.borrow().layout.align,
        ResolvedType::Enum { cell, .. } => cell.borrow().layout.align,
        _ => 1,
    }
}

fn round_up(offset: u32, align: u32) -> u32 {
    if align <= 1 { offset } else { offset.div_ceil(align) * align }
}

/// Places one field at `offset`, honoring both its own transitive alignment
/// (`field_align`, from `type_alignment`) and the enclosing type's own
/// `pack` (see `omega_analyzer::annotations::Layout::pack`'s doc comment):
/// a chunk of size `pack` starts at every multiple of `pack`; a field is
/// placed at its own (already alignment-rounded) offset if it fits in what
/// remains of the chunk it would start in, *or* if it would be the first
/// thing placed in that chunk (`offset_in_chunk == 0` -- without this, a
/// single field bigger than `pack` itself could never "fit" and would
/// uselessly bounce to the next chunk boundary forever); otherwise padding
/// advances to the start of the next chunk. `pack == 1` (the default) is a
/// true no-op: every offset is already a multiple of `1`, so
/// `offset_in_chunk` is always `0` and every field lands at its plain
/// aligned offset -- byte-identical to this type's layout before `@layout`
/// existed at all.
fn place_field(offset: u32, field_align: u32, field_size: u32, pack: u32) -> u32 {
    let aligned = round_up(offset, field_align);
    let chunk_start = (aligned / pack) * pack;
    let offset_in_chunk = aligned - chunk_start;
    if offset_in_chunk == 0 || offset_in_chunk + field_size <= pack {
        aligned
    } else {
        round_up(chunk_start + pack, field_align)
    }
}

/// The alignment *shift* `StackSlotData::new` wants (2^shift bytes) for a
/// stack slot holding a value whose own required alignment (`type_alignment`)
/// is `align` bytes (always a power of two, or `1` -- see
/// `annotations::resolve_layout`'s validation). Never lower than `4` (16
/// bytes) -- every stack slot's existing baseline -- so this is a pure
/// no-op for the overwhelming common case (no `@layout(align = ...)`
/// anywhere, where `align` is `1`); only a `@layout(align = n)` type with
/// `n > 16` raises it further.
fn stack_align_shift(align: u32) -> u8 {
    align.max(1).ilog2().max(4) as u8
}

/// One field-sequence's full layout -- struct fields, an enum's own
/// `[tag, header..., dynamic...]` run, or a single variant's body fields.
/// Tracks *both* byte offsets (for `Slot`/`Address`-backed memory access --
/// `field_byte_offset`, the enum offset functions below) and leaf-list
/// start indices (for `Values`-backed/register access --
/// `project_field_access`, the `EnumHeader`/`EnumDynamicField` projection
/// arms): once an `@layout(align = n)`/`@layout(pack = n)` field can insert
/// a gap, the two stop being derivable from each other by a flat per-field
/// leaf-count sum, so both are computed together, once, here -- the single
/// source of truth every other layout function in this file reads from, so
/// none of them can drift out of agreement with each other or with
/// `into_ir_type`'s own `Struct`/`Enum` arms (which use this directly).
struct FieldLayout {
    byte_offsets: Vec<u32>,
    leaf_starts: Vec<usize>,
    leaves: Vec<IRType>,
    /// The packed-with-interior-layout running offset just past the last
    /// field -- *not* yet rounded up to the whole sequence's own trailing
    /// alignment (callers embedding a struct/enum's own `@layout(align =
    /// n)` round this up themselves; an enum's tag/header/dynamic run has
    /// no trailing alignment of its own at all, only the payload's start
    /// and the enum's overall end do).
    packed_end: u32,
}

/// `pack` is the *enclosing* struct/enum's own resolved `@layout(pack =
/// ...)` (see `place_field`) -- applied uniformly to every field in `types`,
/// whether this call is laying out a struct's own fields, an enum's
/// `[tag, header..., dynamic...]` run, or one variant's body fields.
fn layout_fields(types: &[ResolvedType], pack: u32, codegen: &Codegen) -> FieldLayout {
    let mut byte_offsets = Vec::with_capacity(types.len());
    let mut leaf_starts = Vec::with_capacity(types.len());
    let mut leaves = Vec::new();
    let mut offset = 0u32;
    for r#type in types {
        let field_leaves = r#type.clone().into_ir_type(codegen);
        let field_size = field_leaves.iter().map(|t| t.bytes()).sum::<u32>();
        let placed = place_field(offset, type_alignment(r#type), field_size, pack);
        leaves.extend(std::iter::repeat_n(types::I8, (placed - offset) as usize));
        byte_offsets.push(placed);
        leaf_starts.push(leaves.len());
        offset = placed + field_size;
        leaves.extend(field_leaves);
    }
    FieldLayout { byte_offsets, leaf_starts, leaves, packed_end: offset }
}

/// A `FieldAccess` projection's already-resolved `field_index`'s byte offset
/// within `struct_type` (honoring interior alignment/pack gaps -- see
/// `place_field`) -- the memory-backed (`Slot`/`Address`) counterpart to
/// `project_field_access`'s positional (`Values`) slicing.
fn field_byte_offset(struct_type: &ResolvedStructType, field_index: usize, codegen: &Codegen) -> u32 {
    let field_types: Vec<ResolvedType> = struct_type.fields.iter().map(|(_, t, _)| t.clone()).collect();
    layout_fields(&field_types, struct_type.layout.pack, codegen).byte_offsets[field_index]
}

/// The size of an enum's payload region: the largest variant body, each laid
/// out via `layout_fields` with the enum's own `pack` (so a variant whose
/// own fields need internal alignment/pack-chunking is sized correctly) --
/// `0` for an enum with no variant bodies at all.
fn enum_payload_bytes(enum_type: &ResolvedEnumType, pack: u32, codegen: &Codegen) -> u32 {
    enum_type
        .variants
        .iter()
        .map(|v| {
            let field_types: Vec<ResolvedType> = v.fields.iter().map(|(_, t, _)| t.clone()).collect();
            layout_fields(&field_types, pack, codegen).packed_end
        })
        .max()
        .unwrap_or(0)
}

/// The largest alignment any variant's own body field demands -- every
/// variant's body shares the same starting offset (the payload region is
/// one shared union of all of them), so the payload's own start (see
/// `enum_payload_offset`) has to satisfy whichever variant needs the most.
/// `1` (no alignment) when no variant has any field demanding one.
fn enum_payload_alignment(enum_type: &ResolvedEnumType) -> u32 {
    enum_type.variants.iter().flat_map(|v| v.fields.iter().map(|(_, t, _)| type_alignment(t))).max().unwrap_or(1)
}

/// The enum's own `[tag, header..., dynamic...]` run, laid out (with the
/// enum's own `@layout(pack = ...)`) as one `layout_fields` sequence --
/// shared by every offset function below, so `enum_header_offset`/
/// `enum_dynamic_field_offset`/`enum_payload_offset` (and their
/// `Values`-storage counterparts in the projection-handling code) all index
/// into the exact same layout `into_ir_type`'s `Enum` arm built its leaves
/// from.
fn enum_prefix_layout(enum_type: &ResolvedEnumType, codegen: &Codegen) -> FieldLayout {
    let mut types = vec![enum_type.tag_type.clone()];
    types.extend(enum_type.header.iter().map(|(_, t, _)| t.clone()));
    types.extend(enum_type.dynamic_fields.iter().map(|(_, t, _)| t.clone()));
    layout_fields(&types, enum_type.layout.pack, codegen)
}

/// The size of a union's storage: its largest field, in packed bytes -- `0`
/// for a union with no fields at all. See `enum_payload_bytes`, whose shape
/// this mirrors exactly (a union's whole body plays the same role a single
/// enum variant's body does).
fn union_bytes(union_type: &ResolvedUnionType, codegen: &Codegen) -> u32 {
    union_type.fields.iter().map(|(_, r#type, _)| total_bytes(r#type.clone(), codegen)).max().unwrap_or(0)
}

/// Decomposes an enum's payload region into opaque integer leaves covering
/// exactly `bytes` -- as many `i64`s as fit, then one `i32`/`i16`/`i8` as
/// needed. Deterministic and layout-only: these leaves exist so the payload
/// can ride the same flattened-scalar machinery every other value uses
/// (copies, params, returns); nothing ever interprets them as numbers.
fn payload_chunks(mut bytes: u32) -> Vec<IRType> {
    let mut chunks = Vec::new();
    while bytes >= 8 {
        chunks.push(types::I64);
        bytes -= 8;
    }
    if bytes >= 4 {
        chunks.push(types::I32);
        bytes -= 4;
    }
    if bytes >= 2 {
        chunks.push(types::I16);
        bytes -= 2;
    }
    if bytes >= 1 {
        chunks.push(types::I8);
    }
    chunks
}

/// A header field's byte offset within an enum value (honoring interior
/// alignment/pack gaps) -- past the tag and every preceding header field.
/// Index `1 + index` into `enum_prefix_layout`'s combined run: index `0` is
/// always the tag.
fn enum_header_offset(enum_type: &ResolvedEnumType, index: usize, codegen: &Codegen) -> u32 {
    enum_prefix_layout(enum_type, codegen).byte_offsets[1 + index]
}

/// A shared dynamic field's byte offset within an enum value -- past the
/// tag, the whole header, and every preceding dynamic field. Mirrors
/// `enum_header_offset` exactly, one region further into the same combined
/// run.
fn enum_dynamic_field_offset(enum_type: &ResolvedEnumType, index: usize, codegen: &Codegen) -> u32 {
    enum_prefix_layout(enum_type, codegen).byte_offsets[1 + enum_type.header.len() + index]
}

/// The payload region's byte offset within an enum value -- past the tag,
/// the whole header, and the whole shared-dynamic-fields region, placed (via
/// `place_field`, honoring both the enum's own `pack` and whatever alignment
/// the largest variant field demands -- see `enum_payload_alignment`) right
/// after the prefix run -- every variant's body shares this one starting
/// offset.
fn enum_payload_offset(enum_type: &ResolvedEnumType, codegen: &Codegen) -> u32 {
    let prefix = enum_prefix_layout(enum_type, codegen);
    let payload_size = enum_payload_bytes(enum_type, enum_type.layout.pack, codegen);
    place_field(prefix.packed_end, enum_payload_alignment(enum_type), payload_size, enum_type.layout.pack)
}

/// A body field's byte offset within an enum value: the payload region's
/// start plus every preceding field of the *same variant*, honoring
/// interior alignment/pack gaps within that variant's own fields (each
/// variant's body independently starts at the payload's start -- that's
/// the union).
fn enum_body_field_offset(
    enum_type: &ResolvedEnumType,
    variant_index: usize,
    field_index: usize,
    codegen: &Codegen,
) -> u32 {
    let field_types: Vec<ResolvedType> =
        enum_type.variants[variant_index].fields.iter().map(|(_, t, _)| t.clone()).collect();
    enum_payload_offset(enum_type, codegen)
        + layout_fields(&field_types, enum_type.layout.pack, codegen).byte_offsets[field_index]
}

impl Codegen {
    /// Builds a `TargetIsa` from `target`/`opt_level` and runs the whole
    /// declare-then-define pipeline. The only fallible step is ISA
    /// construction (see the struct's own doc comment for why) -- a `target`
    /// this build of the compiler can't support, or one Cranelift itself
    /// rejects, comes back as a plain `String` (matching `omgc`'s own
    /// CLI-error convention, e.g. `parse_args`'s `Result<Args, String>`)
    /// rather than the panic `isa::lookup_by_name` (a thin `.expect()`
    /// wrapper unsuitable for untrusted CLI input) would have produced.
    pub fn generate(
        module_name: &str,
        target: Target,
        opt_level: OptLevel,
        emit: EmitKind,
        modules: Vec<(Vec<Ident>, MirModule)>,
        entry: &[Ident],
        extern_functions: Vec<ExternFunctionRef>,
    ) -> Result<Self, String> {
        let isa = {
            let mut builder = settings::builder();

            builder.set("opt_level", opt_level.cranelift_setting()).unwrap();
            builder.enable("is_pic").unwrap();

            let flags = settings::Flags::new(builder);

            isa::lookup(target.to_triple())
                .map_err(|e| format!("target '{target}' is not supported by this build of the compiler: {e}"))?
                .finish(flags)
                .map_err(|e| format!("failed to build a code generator for target '{target}': {e}"))?
        };

        let module = {
            let translation_unit_name = module_name.bytes().collect::<Vec<_>>();
            let libcall_names = cranelift_module::default_libcall_names();
            let builder =
                ObjectBuilder::new(isa.clone(), translation_unit_name, libcall_names).unwrap();
            ObjectModule::new(builder)
        };

        let mut codegen = Self {
            isa,
            module,
            functions: HashMap::new(),
            ctx: codegen::Context::new(),
            emit,
            captured_text: String::new(),

            bytes: HashMap::new(),
            vtables: HashMap::new(),

            local_bytes: HashMap::new(),
            local_args: Vec::new(),
            stack_slots: Vec::new(),
            arg_count: 0,
            declared_symbols: HashMap::new(),
            symbol_error: None,
        };

        codegen.update_all(modules, entry, extern_functions);

        if let Some(error) = codegen.symbol_error {
            return Err(error);
        }

        Ok(codegen)
    }

    fn clear_local(&mut self) {
        self.local_bytes.clear();
        self.ctx.clear();
        self.stack_slots.clear();
        self.local_args.clear();
        self.arg_count = 0;
    }

    /// Walks a place's root and projections once, tracking where its
    /// storage currently lives -- switching from `Slot`/`Values` to
    /// `Address` the moment a `Deref` (explicit, or an array `Index`'s
    /// implicit pointer arithmetic) happens, since the pointee isn't known
    /// until runtime. This is the one general mechanism behind reading,
    /// writing, and taking the address of a place, regardless of how many
    /// derefs/field accesses/indices got it there.
    fn resolve_place_storage(
        &mut self,
        place: &MirPlace,
        builder: &mut FunctionBuilder,
    ) -> (PlaceStorage, ResolvedType) {
        let (mut current, mut current_type) = match &place.root {
            MirPlaceRoot::Local { id, r#type } => {
                let current = if (id.0 as usize) < self.arg_count {
                    PlaceStorage::Values(self.local_args[id.0 as usize].clone())
                } else {
                    let slot = match self.stack_slots[id.0 as usize] {
                        Some(slot) => slot,
                        None => {
                            let shift = stack_align_shift(type_alignment(r#type));
                            let size = total_bytes(r#type.clone(), self);
                            let slot = builder
                                .create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, size, shift));
                            self.stack_slots[id.0 as usize] = Some(slot);
                            slot
                        }
                    };
                    PlaceStorage::Slot { slot, offset: 0 }
                };
                (current, r#type.clone())
            }
            MirPlaceRoot::Function(_) => {
                unreachable!(
                    "a function reference is never itself further-projected; calls resolve it directly via get_place_value"
                );
            }
            MirPlaceRoot::Global { r#type, .. } => {
                let _ = r#type;
                todo!("global/extern data storage is not yet implemented")
            }
            // A temporary as the root of a projection chain -- `foo().bar`,
            // `Vec2 { ... }.x`, or a method call's implicit `&self` on
            // either: materialized into an anonymous stack slot so the rest
            // of the projection walk (including taking its address) has
            // ordinary memory to work against, exactly like a local's slot
            // -- the temporary just has no name and no declaration to key
            // a stack slot by.
            MirPlaceRoot::Expr(expr) => {
                let r#type = expr.r#type.clone();
                let values = self.process_expr(builder, (**expr).clone());
                let shift = stack_align_shift(type_alignment(&r#type));
                let size = total_bytes(r#type.clone(), self);
                let slot = builder
                    .create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, size, shift));
                let storage = PlaceStorage::Slot { slot, offset: 0 };
                self.store_scalars(builder, &storage, &values);
                (storage, r#type)
            }
        };

        for projection in &place.projections {
            match projection {
                MirProjection::FieldAccess { index, r#type, .. } => {
                    let ResolvedType::Struct(struct_type) = &current_type else {
                        unreachable!("mir body guarantees field projections are only built against a struct type");
                    };
                    let struct_type = struct_type.clone();
                    let struct_type = struct_type.borrow();
                    current = match current {
                        PlaceStorage::Values(values) => {
                            PlaceStorage::Values(project_field_access(self, &values, &struct_type, *index))
                        }
                        PlaceStorage::Slot { slot, offset } => PlaceStorage::Slot {
                            slot,
                            offset: offset + field_byte_offset(&struct_type, *index, self),
                        },
                        PlaceStorage::Address { base, offset } => PlaceStorage::Address {
                            base,
                            offset: offset + field_byte_offset(&struct_type, *index, self),
                        },
                    };
                    current_type = r#type.clone();
                }

                // Every field lives at offset 0 (see
                // `MirProjection::UnionField`'s doc comment) -- the only
                // real work here is spilling an SSA-value-backed union to
                // memory first (mirrors `EnumBody`'s identical spill, for the
                // identical reason: no leaf slice can reinterpret one field's
                // real shape out of the union's own opaque payload chunks),
                // then letting `current_type` advance to the field's type.
                MirProjection::UnionField { r#type, .. } => {
                    if let PlaceStorage::Values(values) = &current {
                        let shift = stack_align_shift(type_alignment(&current_type));
                        let size = total_bytes(current_type.clone(), self);
                        let slot = builder
                            .create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, size, shift));
                        let spilled = PlaceStorage::Slot { slot, offset: 0 };
                        self.store_scalars(builder, &spilled, &values.clone());
                        current = spilled;
                    }
                    current_type = r#type.clone();
                }

                MirProjection::Deref { r#type } => {
                    let ptr_value = self.load_scalars(builder, &current, &current_type)[0];
                    current = PlaceStorage::Address { base: ptr_value, offset: 0 };
                    current_type = r#type.clone();
                }

                MirProjection::Index { index_expr, item_type } => {
                    // The element size comes from `item_type` (the resolved
                    // element type analysis already picked out), not from
                    // flattening `current_type` itself -- the container's own
                    // `into_ir_type` (a single thin pointer for `Array`, or
                    // N*item leaves for `SizedArray`) has nothing to do with
                    // one element's size.
                    let element_ir_size = total_bytes(item_type.clone(), self);

                    let mut base = match &current_type {
                        // Inline contiguous storage: index off the storage's
                        // own address, not a pointer value loaded from it --
                        // there is no pointer to load, the elements live
                        // directly in `current`.
                        ResolvedType::SizedArray(_, _) => self.place_storage_address(builder, &current),
                        // `Array` (the legacy thin-pointer unsized form,
                        // e.g. `argv`) *is* a pointer value; `Slice`/`Str`'s
                        // first flattened leaf is its data pointer (the
                        // second, its length, isn't needed for a
                        // single-element index) -- identical leaf layout,
                        // so the same one-leaf load works for both.
                        ResolvedType::Array(_) | ResolvedType::Slice { .. } | ResolvedType::Str { .. } => {
                            self.load_scalars(builder, &current, &current_type)[0]
                        }
                        _ => unreachable!(
                            "mir body guarantees Index projections only apply to Array/SizedArray/Slice/Str"
                        ),
                    };
                    let mut index = self.process_expr(builder, (**index_expr).clone())[0];

                    let ptr_type = self.pointer_type();
                    if builder.func.dfg.value_type(base) != ptr_type {
                        base = builder.ins().uextend(ptr_type, base);
                    }
                    if builder.func.dfg.value_type(index) != ptr_type {
                        index = builder.ins().uextend(ptr_type, index);
                    }

                    let element_size = builder.ins().iconst(ptr_type, element_ir_size as i64);
                    let offset = builder.ins().imul(index, element_size);
                    let element_addr = builder.ins().iadd(base, offset);

                    current = PlaceStorage::Address { base: element_addr, offset: 0 };
                    current_type = item_type.clone();
                }

                MirProjection::EnumTag { r#type } => {
                    // The tag is the leading leaf/bytes of every enum value
                    // -- offset 0, first leaf.
                    let ResolvedType::Enum { cell, .. } = &current_type else {
                        unreachable!("mir body guarantees EnumTag projections are only built against an enum type");
                    };
                    let tag_leaves = cell.borrow().tag_type.clone().into_ir_type(self).len();
                    current = match current {
                        PlaceStorage::Values(values) => PlaceStorage::Values(values[..tag_leaves].to_vec()),
                        memory_backed => memory_backed,
                    };
                    current_type = r#type.clone();
                }

                MirProjection::EnumHeader { index, r#type, .. } => {
                    let ResolvedType::Enum { cell, .. } = &current_type else {
                        unreachable!("mir body guarantees EnumHeader projections are only built against an enum type");
                    };
                    let cell = cell.clone();
                    let enum_type = cell.borrow();
                    current = match current {
                        PlaceStorage::Values(values) => {
                            // Positional, by leaf-list start index -- shares
                            // `enum_header_offset`'s exact layout (see
                            // `enum_prefix_layout`), so an interior gap
                            // before this field (from an earlier field's own
                            // alignment demand) is accounted for here too.
                            let start = enum_prefix_layout(&enum_type, self).leaf_starts[1 + *index];
                            let len = enum_type.header[*index].1.clone().into_ir_type(self).len();
                            PlaceStorage::Values(values[start..start + len].to_vec())
                        }
                        PlaceStorage::Slot { slot, offset } => PlaceStorage::Slot {
                            slot,
                            offset: offset + enum_header_offset(&enum_type, *index, self),
                        },
                        PlaceStorage::Address { base, offset } => PlaceStorage::Address {
                            base,
                            offset: offset + enum_header_offset(&enum_type, *index, self),
                        },
                    };
                    current_type = r#type.clone();
                }

                // `EnumHeader`'s arm above, mirrored exactly -- the only
                // difference between the two is which offset helper and
                // field list to read from (dynamic fields sit right after
                // the header); mutability is handled generically wherever a
                // place is written to, not here.
                MirProjection::EnumDynamicField { index, r#type, .. } => {
                    let ResolvedType::Enum { cell, .. } = &current_type else {
                        unreachable!("mir body guarantees EnumDynamicField projections are only built against an enum type");
                    };
                    let cell = cell.clone();
                    let enum_type = cell.borrow();
                    current = match current {
                        PlaceStorage::Values(values) => {
                            // See `EnumHeader`'s identical `Values` arm above.
                            let start =
                                enum_prefix_layout(&enum_type, self).leaf_starts[1 + enum_type.header.len() + *index];
                            let len = enum_type.dynamic_fields[*index].1.clone().into_ir_type(self).len();
                            PlaceStorage::Values(values[start..start + len].to_vec())
                        }
                        PlaceStorage::Slot { slot, offset } => PlaceStorage::Slot {
                            slot,
                            offset: offset + enum_dynamic_field_offset(&enum_type, *index, self),
                        },
                        PlaceStorage::Address { base, offset } => PlaceStorage::Address {
                            base,
                            offset: offset + enum_dynamic_field_offset(&enum_type, *index, self),
                        },
                    };
                    current_type = r#type.clone();
                }

                MirProjection::EnumBody { variant_index, field_index, r#type } => {
                    let ResolvedType::Enum { cell, .. } = &current_type else {
                        unreachable!("mir body guarantees EnumBody projections are only built against an enum type");
                    };
                    let cell = cell.clone();
                    // A body field lives inside the opaque payload chunks,
                    // which no leaf slice can address -- an SSA-value-backed
                    // enum (a parameter) is spilled to an anonymous slot
                    // first, exactly like a temporary place root, so the
                    // field is an ordinary byte offset from there.
                    if let PlaceStorage::Values(values) = &current {
                        let shift = stack_align_shift(type_alignment(&current_type));
                        let size = total_bytes(current_type.clone(), self);
                        let slot = builder
                            .create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, size, shift));
                        let spilled = PlaceStorage::Slot { slot, offset: 0 };
                        self.store_scalars(builder, &spilled, &values.clone());
                        current = spilled;
                    }
                    let field_offset =
                        enum_body_field_offset(&cell.borrow(), *variant_index, *field_index, self);
                    current = match current {
                        PlaceStorage::Slot { slot, offset } => {
                            PlaceStorage::Slot { slot, offset: offset + field_offset }
                        }
                        PlaceStorage::Address { base, offset } => {
                            PlaceStorage::Address { base, offset: offset + field_offset }
                        }
                        PlaceStorage::Values(_) => unreachable!("spilled to a slot above"),
                    };
                    current_type = r#type.clone();
                }

                MirProjection::SliceLength => {
                    // A slice is flattened as [data pointer, i32 length] (see
                    // `ResolvedType::Slice`'s `into_ir_type`) -- `.length` is
                    // just the second leaf, at a byte offset of one pointer's
                    // width past the start of the slice's own storage.
                    let ptr_size = self.pointer_type().bytes();
                    current = match current {
                        PlaceStorage::Values(values) => PlaceStorage::Values(vec![values[1]]),
                        PlaceStorage::Slot { slot, offset } => {
                            PlaceStorage::Slot { slot, offset: offset + ptr_size }
                        }
                        PlaceStorage::Address { base, offset } => {
                            PlaceStorage::Address { base, offset: offset + ptr_size }
                        }
                    };
                    current_type = ResolvedType::I32;
                }
            }
        }

        (current, current_type)
    }

    /// The runtime address backing `storage` -- the same address-resolution
    /// `AddressOf` needs, but also needed by `SizedArray` indexing (which
    /// must index off the storage's own address, having no pointer value to
    /// load) and slice construction from a `SizedArray` base.
    fn place_storage_address(&mut self, builder: &mut FunctionBuilder, storage: &PlaceStorage) -> Value {
        let ptr_type = self.pointer_type();
        match storage {
            PlaceStorage::Values(_) => {
                todo!("taking the address of a function parameter is not yet implemented");
            }
            PlaceStorage::Slot { slot, offset } => builder.ins().stack_addr(ptr_type, *slot, *offset as i32),
            PlaceStorage::Address { base, offset: 0 } => *base,
            PlaceStorage::Address { base, offset } => {
                let offset_val = builder.ins().iconst(ptr_type, *offset as i64);
                builder.ins().iadd(*base, offset_val)
            }
        }
    }

    /// Reads every scalar leaf of `r#type` out of `storage`, in leaf order.
    fn load_scalars(
        &mut self,
        builder: &mut FunctionBuilder,
        storage: &PlaceStorage,
        r#type: &ResolvedType,
    ) -> Vec<Value> {
        if let PlaceStorage::Values(values) = storage {
            return values.clone();
        }

        let mut result = Vec::new();
        let mut rel_offset = 0u32;
        for leaf in r#type.clone().into_ir_type(self) {
            let value = match storage {
                PlaceStorage::Slot { slot, offset } => {
                    builder.ins().stack_load(leaf, *slot, (*offset + rel_offset) as i32)
                }
                PlaceStorage::Address { base, offset } => {
                    builder.ins().load(leaf, MemFlags::new(), *base, (*offset + rel_offset) as i32)
                }
                PlaceStorage::Values(_) => unreachable!("handled above"),
            };
            result.push(value);
            rel_offset += leaf.bytes();
        }
        result
    }

    /// Writes `values` (one per scalar leaf, in leaf order) into `storage`.
    fn store_scalars(&mut self, builder: &mut FunctionBuilder, storage: &PlaceStorage, values: &[Value]) {
        let mut rel_offset = 0u32;
        for value in values {
            let leaf = builder.func.dfg.value_type(*value);
            match storage {
                PlaceStorage::Values(_) => {
                    todo!("assignment into a function parameter is not yet implemented");
                }
                PlaceStorage::Slot { slot, offset } => {
                    builder.ins().stack_store(*value, *slot, (*offset + rel_offset) as i32);
                }
                PlaceStorage::Address { base, offset } => {
                    builder.ins().store(MemFlags::new(), *value, *base, (*offset + rel_offset) as i32);
                }
            }
            rel_offset += leaf.bytes();
        }
    }

    fn get_place_value(&mut self, place: &MirPlace, builder: &mut FunctionBuilder) -> Vec<Value> {
        // A function reference has no memory backing at all -- just a
        // symbol address -- so it's handled before the general
        // storage-resolution path (mir guarantees this root never carries
        // further projections, see `resolve_place_storage`).
        if let MirPlaceRoot::Function(decl_id) = &place.root {
            let function = *self.functions.get(decl_id).unwrap_or_else(|| {
                panic!("mir body guarantees {decl_id:?} was declared as a function before this use")
            });
            let func = self.get_func_ref_from_id(builder, function);
            return vec![builder.ins().func_addr(self.pointer_type(), func)];
        }

        let (storage, r#type) = self.resolve_place_storage(place, builder);
        self.load_scalars(builder, &storage, &r#type)
    }

    /// Whether `return_type` is returned through a hidden `StructReturn`
    /// pointer instead of in registers: x86_64 SysV has exactly two integer
    /// return registers (rax/rdx), so any value flattening to more than two
    /// leaves -- a large struct, or any enum with a payload -- can't come
    /// back by value. (Two int + two float leaves would technically still
    /// fit, but classifying leaf register classes buys nothing over this
    /// simple, always-correct rule.) Must agree between definitions and
    /// call sites -- both derive their `Signature` from `make_function_sig`,
    /// so it's decided in exactly one place.
    fn needs_sret(&self, return_type: &ResolvedType) -> bool {
        return_type.clone().into_ir_type(self).len() > 2
    }

    fn make_function_sig(&self, resolved_fntype: ResolvedFunctionType) -> Signature {
        let mut sig = self.module.make_signature();

        // The hidden struct-return pointer is always the first parameter
        // (see `needs_sret`); cranelift itself handles the SysV requirement
        // of also returning that pointer in rax, so the signature declares
        // no return values at all in this case.
        if *resolved_fntype.return_type != ResolvedType::Void {
            if self.needs_sret(&resolved_fntype.return_type) {
                sig.params
                    .push(AbiParam::special(self.pointer_type(), ArgumentPurpose::StructReturn));
            } else {
                for leaf in resolved_fntype.return_type.into_ir_type(self) {
                    sig.returns.push(AbiParam::new(leaf));
                }
            }
        }

        let ir_params = resolved_fntype
            .params
            .into_iter()
            .flat_map(|param| param.1.into_ir_type(self));
        for param in ir_params {
            sig.params.push(AbiParam::new(param));
        }

        if resolved_fntype.is_variadic {
            sig.call_conv = isa::CallConv::SystemV;
        }

        sig
    }

    fn update_extern_decl(&mut self, extern_decl: MirExternDeclaration) {
        match extern_decl.r#type {
            ResolvedType::Function(resolved_fntype) => {
                let sig = self.make_function_sig(resolved_fntype);

                let function_id = self
                    .module
                    .declare_function(&extern_decl.ident.0, Linkage::Import, &sig)
                    .unwrap();

                self.functions.insert(extern_decl.id, function_id);
            }

            _ => todo!("extern data declarations (non-function externs) are not yet implemented"),
        }
    }

    /// An anonymous data object's symbol -- a pure function of its own
    /// bytes, not an arbitrary per-process counter: two identical
    /// constants, in the same compilation or two separate ones, always
    /// name themselves identically, the same "stable, type/content-derived
    /// name" property `omega_mangle` gives real functions/methods -- see
    /// its own design notes for why that matters. Rapidhash V3
    /// (`rapidhash::v3::rapidhash_v3`, avalanche-enabled -- its default,
    /// matching the reference C++ implementation, chosen over the crate's
    /// own `fast` preset since that one deliberately trades away mixing
    /// quality for hashmap-bucket-selection speed, a different need than a
    /// standalone, collision-averse identifier) is a deliberately
    /// non-cryptographic choice: nothing here is adversarial (the input is
    /// always the compiler's own already-resolved constant data), so all
    /// that's needed is a fast hash with a low *accidental* collision rate
    /// at realistic program sizes, not preimage/collision resistance
    /// against a deliberate attacker.
    fn data_symbol(bytes: &[u8]) -> String {
        format!("_omgdata_{:016x}", rapidhash::v3::rapidhash_v3(bytes))
    }

    /// Declares (and defines) `s`'s bytes as an anonymous module-level data
    /// object, verbatim -- no null terminator, shared by `"..."` (`*str`)
    /// and `b"..."` (`*[u8]`) literals alike (see `bytes`'s own doc
    /// comment for why one function/map correctly serves both now).
    fn get_or_declare_global_bytes(&mut self, s: String) -> DataId {
        let bytes = s.clone().into_bytes();
        let sym = Self::data_symbol(&bytes);
        let id = self
            .module
            .declare_data(&sym, Linkage::Preemptible, false, false)
            .unwrap();

        let mut desc = DataDescription::new();
        desc.define(bytes.into_boxed_slice());
        self.module.define_data(id, &desc).unwrap();

        self.bytes.insert(s, id);

        id
    }

    fn get_func_ref_from_id(&mut self, builder: &mut FunctionBuilder, func_id: FuncId) -> FuncRef {
        self.module.declare_func_in_func(func_id, builder.func)
    }

    /// A byte-run constant's two-leaf `[pointer, length]` form -- the
    /// shape both `ResolvedType::Slice` and `ResolvedType::Str`'s
    /// `into_ir_type` expect (identical for both) -- deduplicated per
    /// module (`bytes`) and per function (`local_bytes`, which only
    /// caches the pointer; the length is a cheap `iconst` recomputed each
    /// call, same as any other compile-time-constant length). Shared by
    /// string literal expressions, byte-string literal expressions, and
    /// enum header/dynamic-field constants -- the caller alone decides
    /// whether the surrounding value is typed `*str` or `*[u8]`.
    fn emit_bytes(&mut self, builder: &mut FunctionBuilder, s: String) -> Vec<Value> {
        let len = builder.ins().iconst(types::I32, s.len() as i64);

        if let Some(local_value) = self.local_bytes.get(&s) {
            return vec![*local_value, len];
        }

        let ptr_type = self.pointer_type();
        let data_id = if let Some(id) = self.bytes.get(&s) {
            *id
        } else {
            self.get_or_declare_global_bytes(s.clone())
        };

        let global_value = self.module.declare_data_in_func(data_id, builder.func);
        let ptr = builder.ins().global_value(ptr_type, global_value);

        self.local_bytes.insert(s, ptr);

        vec![ptr, len]
    }

    /// Emits one `ConstValue` (an enum tag/header constant, or a
    /// `MirExpr::ConstSlice`) as its leaves, in leaf order -- every
    /// variant but `Slice`/`Array` is exactly one IR leaf; `Slice` is the
    /// two-leaf `[ptr, len]` fat pointer every other `ResolvedType::Slice`
    /// value already is (see `emit_const_slice`); `Array` is every
    /// element's own leaves concatenated in order, with no indirection at
    /// all -- the same packed, no-padding layout `into_ir_type`'s
    /// `SizedArray` case already flattens to.
    fn emit_const_value(&mut self, builder: &mut FunctionBuilder, value: &ConstValue, r#type: &ResolvedType) -> Vec<Value> {
        match value {
            ConstValue::Number(number) => {
                let leaf = r#type.clone().into_ir_type(self)[0];
                vec![match number {
                    NumberValue::Signed(v) => builder.ins().iconst(leaf, *v),
                    NumberValue::Unsigned(v) => builder.ins().iconst(leaf, *v as i64),
                    NumberValue::Float(v) if leaf == types::F32 => builder.ins().f32const(*v as f32),
                    NumberValue::Float(v) => builder.ins().f64const(*v),
                }]
            }
            ConstValue::Bool(b) => vec![builder.ins().iconst(types::I8, *b as i64)],
            ConstValue::Char(c) => vec![builder.ins().iconst(types::I32, *c as i64)],
            ConstValue::Str(s) => self.emit_bytes(builder, s.clone()),
            ConstValue::Slice(elements) => {
                let ResolvedType::Slice { item, .. } = r#type else {
                    unreachable!("mir body guarantees a Slice constant's own type is Slice");
                };
                self.emit_const_slice(builder, elements, item)
            }
            ConstValue::Array(elements) => {
                let ResolvedType::SizedArray(item, _) = r#type else {
                    unreachable!("mir body guarantees an Array constant's own type is SizedArray");
                };
                let mut values = Vec::with_capacity(elements.len());
                for element in elements {
                    values.extend(self.emit_const_value(builder, element, item));
                }
                values
            }
        }
    }

    /// A compile-time slice's `[ptr, len]` leaves -- unlike `emit_bytes`,
    /// deliberately *not* deduplicated across call sites:
    /// `ConstValue` isn't cheaply hashable (it nests, and `NumberValue::Float`
    /// has no total order), and each occurrence is a one-shot codegen site
    /// (one enum construction, or one `&[...]` expression) rather than
    /// something plausibly repeated many times per function the way string
    /// literals are.
    fn emit_const_slice(&mut self, builder: &mut FunctionBuilder, elements: &[ConstValue], item_type: &ResolvedType) -> Vec<Value> {
        let ptr_type = self.pointer_type();
        let len = builder.ins().iconst(types::I32, elements.len() as i64);
        let data_id = self.build_const_slice_data(elements, item_type);
        let global_value = self.module.declare_data_in_func(data_id, builder.func);
        let ptr = builder.ins().global_value(ptr_type, global_value);
        vec![ptr, len]
    }

    /// Builds one anonymous, module-level data object holding `elements`
    /// laid out at consecutive `total_bytes(item_type)`-sized slots -- the
    /// same packed layout `into_ir_type`'s `SizedArray` case already uses,
    /// so the result is byte-for-byte what an ordinary runtime slice over
    /// this data would expect. `&[]` never reaches here -- `EmptyArrayLiteral`
    /// already rejects it at analysis, same as a bare `[]`.
    fn build_const_slice_data(&mut self, elements: &[ConstValue], item_type: &ResolvedType) -> DataId {
        let stride = total_bytes(item_type.clone(), self);
        let mut bytes = vec![0u8; stride as usize * elements.len()];
        let mut desc = DataDescription::new();
        for (i, element) in elements.iter().enumerate() {
            self.write_const_element(&mut desc, &mut bytes, i as u32 * stride, element, item_type);
        }
        desc.define(bytes.into_boxed_slice());

        let mut hash_input = Vec::new();
        for element in elements {
            self.hash_const_element(&mut hash_input, element, item_type);
        }
        let sym = Self::data_symbol(&hash_input);
        let id = self.module.declare_data(&sym, Linkage::Preemptible, false, false).unwrap();
        self.module.define_data(id, &desc).unwrap();
        id
    }

    /// Appends `value`'s canonical, unambiguous content bytes to `out`,
    /// purely for `data_symbol`'s naming purposes -- deliberately
    /// *not* the same bytes `write_const_element` writes into the real
    /// data object. `write_const_element` leaves a pointer-shaped
    /// element (`Str`, nested `Slice`) as a zero placeholder in the
    /// physical buffer -- the actual target only exists as a
    /// `write_data_addr` relocation recorded in `desc`, invisible to a
    /// hash over raw bytes alone. Hashing the physical buffer directly
    /// would therefore let two constant slices that point at *different*
    /// strings collide on one symbol name whenever their non-pointer
    /// bytes happen to coincide (e.g. `&["a"]` and `&["b"]`, both a
    /// single same-length string) -- harmless under today's `Local`
    /// linkage, but a real silent miscompile risk if these ever move to
    /// weak/COMDAT linkage (two genuinely different constants folded
    /// into one because the linker trusted a colliding name). So this
    /// walks the *logical* `ConstValue` tree instead, writing a
    /// string's real bytes (length-prefixed, since it's the only
    /// variable-length leaf here) rather than a placeholder. Every
    /// other leaf is fixed-width (given `r#type`, shared across one
    /// call) or already length-prefixed (`Slice`'s element count), so
    /// the whole traversal is self-delimiting with no separators needed.
    fn hash_const_element(&mut self, out: &mut Vec<u8>, value: &ConstValue, r#type: &ResolvedType) {
        match value {
            ConstValue::Number(number) => {
                let leaf_bytes = r#type.clone().into_ir_type(self)[0].bytes();
                let raw: u64 = match number {
                    NumberValue::Signed(v) => *v as u64,
                    NumberValue::Unsigned(v) => *v,
                    NumberValue::Float(v) if leaf_bytes == 4 => (*v as f32).to_bits() as u64,
                    NumberValue::Float(v) => v.to_bits(),
                };
                out.extend_from_slice(&raw.to_le_bytes()[..leaf_bytes as usize]);
            }
            ConstValue::Bool(b) => out.push(*b as u8),
            ConstValue::Char(c) => out.extend_from_slice(&(*c as u32).to_le_bytes()),
            ConstValue::Str(s) => {
                out.extend_from_slice(&(s.len() as u64).to_le_bytes());
                out.extend_from_slice(s.as_bytes());
            }
            ConstValue::Slice(nested) => {
                let ResolvedType::Slice { item, .. } = r#type else {
                    unreachable!("mir body guarantees a nested Slice constant's own type is Slice");
                };
                out.extend_from_slice(&(nested.len() as u32).to_le_bytes());
                for element in nested {
                    self.hash_const_element(out, element, item);
                }
            }
            ConstValue::Array(elements) => {
                let ResolvedType::SizedArray(item, _) = r#type else {
                    unreachable!("mir body guarantees a nested Array constant's own type is SizedArray");
                };
                for element in elements {
                    self.hash_const_element(out, element, item);
                }
            }
        }
    }

    /// Writes one element's leaves into `bytes`/`desc` at `offset`. A scalar
    /// (`Number`/`Bool`/`Char`) is written as literal little-endian bytes --
    /// its address never depends on the linker. A pointer-shaped element
    /// (`Str`, or a nested `Slice`) can't have its address known until
    /// link/load time, so it's written as a `DataDescription::write_data_addr`
    /// relocation into its own (recursively built, for `Slice`) data object
    /// instead -- the same "embed a pointer to other static data" mechanism
    /// object file formats already support for e.g. initialized pointer
    /// tables. A nested `Slice` element's trailing length leaf has no such
    /// address dependency, so it's still a literal byte write.
    fn write_const_element(
        &mut self,
        desc: &mut DataDescription,
        bytes: &mut [u8],
        offset: u32,
        value: &ConstValue,
        r#type: &ResolvedType,
    ) {
        match value {
            ConstValue::Number(number) => {
                let leaf_bytes = r#type.clone().into_ir_type(self)[0].bytes();
                let raw: u64 = match number {
                    NumberValue::Signed(v) => *v as u64,
                    NumberValue::Unsigned(v) => *v,
                    NumberValue::Float(v) if leaf_bytes == 4 => (*v as f32).to_bits() as u64,
                    NumberValue::Float(v) => v.to_bits(),
                };
                let start = offset as usize;
                bytes[start..start + leaf_bytes as usize].copy_from_slice(&raw.to_le_bytes()[..leaf_bytes as usize]);
            }
            ConstValue::Bool(b) => bytes[offset as usize] = *b as u8,
            ConstValue::Char(c) => {
                let start = offset as usize;
                bytes[start..start + 4].copy_from_slice(&(*c as u32).to_le_bytes());
            }
            ConstValue::Str(s) => {
                let str_id =
                    if let Some(id) = self.bytes.get(s) { *id } else { self.get_or_declare_global_bytes(s.clone()) };
                let global_value = self.module.declare_data_in_data(str_id, desc);
                desc.write_data_addr(offset, global_value, 0);

                // `*str` (unlike the old, always-null-terminated `*u8` this
                // used to be) is a fat pointer -- the length leaf needs
                // writing too, exactly like `ConstValue::Slice` below.
                let ptr_bytes = self.pointer_type().bytes();
                let len_start = (offset + ptr_bytes) as usize;
                bytes[len_start..len_start + 4].copy_from_slice(&(s.len() as i32).to_le_bytes());
            }
            ConstValue::Slice(nested) => {
                let ResolvedType::Slice { item, .. } = r#type else {
                    unreachable!("mir body guarantees a nested Slice constant's own type is Slice");
                };
                let nested_id = self.build_const_slice_data(nested, item);
                let global_value = self.module.declare_data_in_data(nested_id, desc);
                desc.write_data_addr(offset, global_value, 0);

                let ptr_bytes = self.pointer_type().bytes();
                let len_start = (offset + ptr_bytes) as usize;
                bytes[len_start..len_start + 4].copy_from_slice(&(nested.len() as i32).to_le_bytes());
            }
            // No indirection at all (unlike `Slice`/`Str` above) -- every
            // element is written inline, back to back, into this same
            // buffer, exactly like `emit_const_value`'s `Array` case does
            // for the function-local (non-static-data) form.
            ConstValue::Array(elements) => {
                let ResolvedType::SizedArray(item, _) = r#type else {
                    unreachable!("mir body guarantees a nested Array constant's own type is SizedArray");
                };
                let stride = total_bytes(item.as_ref().clone(), self);
                for (i, element) in elements.iter().enumerate() {
                    self.write_const_element(desc, bytes, offset + i as u32 * stride, element, item);
                }
            }
        }
    }

    /// `concrete`'s own declaring `HirId` -- the identity half of a
    /// vtable's `(concrete type, spec)` cache key. Every spec-object
    /// coercion's pointee is always a struct/enum/union (see
    /// `Analyzer::coerce_to_expected`) -- nothing else ever implements a
    /// spec.
    fn concrete_type_id(concrete: &ResolvedType) -> HirId {
        match concrete {
            ResolvedType::Struct(cell) => cell.borrow().id,
            ResolvedType::Enum { cell, .. } => cell.borrow().id,
            ResolvedType::Union(cell) => cell.borrow().id,
            other => unreachable!("a spec-object coercion's concrete pointee is always struct/enum/union, found {other}"),
        }
    }

    /// `concrete`'s own name, plus every method it already carries (name +
    /// declaring `HirId`, including a spec-default instantiation -- by the
    /// time codegen runs, one is indistinguishable from an ordinary
    /// override; see `Analyzer::resolve_implements_clause`) -- everything
    /// `vtable_for` needs to resolve each of the spec's flattened slot
    /// names to a concrete `FuncId`.
    fn concrete_type_name_and_functions(concrete: &ResolvedType) -> (Ident, Vec<(Ident, HirId)>) {
        let functions = |fs: &[(Ident, omega_analyzer::resolved_type::ResolvedMethod)]| {
            fs.iter().map(|(name, m)| (name.clone(), m.decl_id)).collect()
        };
        match concrete {
            ResolvedType::Struct(cell) => {
                let cell = cell.borrow();
                (cell.name.clone(), functions(&cell.functions))
            }
            ResolvedType::Enum { cell, .. } => {
                let cell = cell.borrow();
                (cell.name.clone(), functions(&cell.functions))
            }
            ResolvedType::Union(cell) => {
                let cell = cell.borrow();
                (cell.name.clone(), functions(&cell.functions))
            }
            other => unreachable!("a spec-object coercion's concrete pointee is always struct/enum/union, found {other}"),
        }
    }

    /// `spec`'s full, ordered, deduplicated slot-name list -- dependencies
    /// (in declaration order) before `spec`'s own functions, first-seen
    /// name wins -- structurally identical to (and must stay in lockstep
    /// with) `Analyzer::flatten_spec`'s own walk, which is what decided
    /// `MirDynamicCall::slot_index` for every call through this same
    /// spec. Unlike `flatten_spec`, this never needs to resolve a raw
    /// signature or detect a conflict: by the time codegen runs, the
    /// program already passed analysis, so every name collision here is
    /// already known-identical.
    fn flatten_spec_slot_names(spec: &Rc<RefCell<ResolvedSpecType>>) -> Vec<Ident> {
        let mut out = Vec::new();
        Self::flatten_spec_slot_names_into(spec, &mut out);
        out
    }

    fn flatten_spec_slot_names_into(spec: &Rc<RefCell<ResolvedSpecType>>, out: &mut Vec<Ident>) {
        let spec = spec.borrow();
        for (dependency, _) in &spec.dependencies {
            Self::flatten_spec_slot_names_into(dependency, out);
        }
        for (name, _) in &spec.functions {
            if !out.contains(name) {
                out.push(name.clone());
            }
        }
    }

    /// Lazily builds (and memoizes) the vtable data object for `(concrete,
    /// spec)` -- a compiler-generated, module-level array of function
    /// pointers, one per slot in `flatten_spec_slot_names`'s order, each
    /// pointing at `concrete`'s own already-declared method for that name
    /// (mirrors `build_const_slice_data`'s static-data-with-relocations
    /// shape exactly, just relocating to function symbols via
    /// `declare_func_in_data`/`write_function_addr` instead of
    /// `declare_data_in_data`/`write_data_addr`). `concrete`'s methods are
    /// guaranteed already `declare_item`'d (never yet *defined* -- codegen
    /// visits every item's declarations before any body, see
    /// `declare_item`'s own doc comment) by the time any expression
    /// (necessarily inside some function body) could coerce it, so
    /// `self.functions` always already has every `FuncId` this needs.
    fn vtable_for(&mut self, concrete: &ResolvedType, spec: &Rc<RefCell<ResolvedSpecType>>) -> DataId {
        let key = (Self::concrete_type_id(concrete), spec.borrow().id);
        if let Some(&id) = self.vtables.get(&key) {
            return id;
        }

        let slot_names = Self::flatten_spec_slot_names(spec);
        let (concrete_name, concrete_functions) = Self::concrete_type_name_and_functions(concrete);

        let ptr_bytes = self.pointer_type().bytes();
        let bytes = vec![0u8; slot_names.len() * ptr_bytes as usize];
        let mut desc = DataDescription::new();
        for (i, name) in slot_names.iter().enumerate() {
            let decl_id = concrete_functions
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, id)| *id)
                .unwrap_or_else(|| {
                    panic!("mir body guarantees '{concrete_name}' provides '{name}' (required by spec '{}')", spec.borrow().name.as_ref())
                });
            let func_id = *self.functions.get(&decl_id).expect("every method is declared before any vtable needs it");
            let fref = self.module.declare_func_in_data(func_id, &mut desc);
            desc.write_function_addr(i as u32 * ptr_bytes, fref);
        }
        desc.define(bytes.into_boxed_slice());

        // `Preemptible` (weak), not `Local`, for the same reason a generic
        // instantiation's own symbol is (see `linkage_for`): a vtable's
        // content is a pure function of `(concrete, spec)` -- relocations
        // to method symbols whose own names are themselves stable, in a
        // slot order `flatten_spec_slot_names` derives deterministically
        // from the spec's own declaration -- so two separate compilations
        // that both coerce the same concrete type to the same spec are
        // guaranteed to build byte-identical vtables under the identical
        // name, and are just as safe (and worth) folding into one copy at
        // link time as a generic function/method instantiation is.
        let symbol = mangle::encode(&mangle::vtable_symbol(concrete, &spec.borrow().name));
        let data_id = self.module.declare_data(&symbol, Linkage::Preemptible, false, false).unwrap();
        self.module.define_data(data_id, &desc).unwrap();

        self.vtables.insert(key, data_id);
        data_id
    }

    /// C's variadic calling convention requires the caller to promote each
    /// *variadic* argument (never a fixed/named one, whose width is fixed by
    /// the callee's prototype) before passing it: any integer narrower than
    /// `int` is sign/zero-extended to 32 bits, and `float` is promoted to
    /// `double` -- otherwise a callee like `printf` (which reads variadic
    /// arguments according to those default-promoted widths, per its format
    /// string) would read garbage. Only applies to `arg_type`s that flatten
    /// to exactly one IR leaf (every numeric primitive does); called
    /// unconditionally on every variadic argument, so anything else (a
    /// pointer, already the right width) just passes through unchanged.
    fn promote_variadic_arg(&mut self, builder: &mut FunctionBuilder, value: Value, arg_type: &ResolvedType) -> Value {
        match arg_type.numeric_kind() {
            Some(NumericKind::Float(width)) if width < 64 => builder.ins().fpromote(types::F64, value),
            Some(NumericKind::Signed(width)) if width < 32 => builder.ins().sextend(types::I32, value),
            Some(NumericKind::Unsigned(width)) if width < 32 => builder.ins().uextend(types::I32, value),
            // `Bool` isn't `numeric_kind`-classified (see its doc comment),
            // but it's still an 8-bit integer that needs the same promotion.
            None if *arg_type == ResolvedType::Bool => builder.ins().uextend(types::I32, value),
            _ => value,
        }
    }

    fn process_expr(&mut self, builder: &mut FunctionBuilder, node: MirExprNode) -> Vec<Value> {
        match node.kind {
            MirExpr::String(s) => self.emit_bytes(builder, s),
            MirExpr::ByteString(s) => self.emit_bytes(builder, s),
            MirExpr::ConstSlice(value) => self.emit_const_value(builder, &value, &node.r#type),

            MirExpr::FunctionCall(MirFunctionCall { callee, fn_type, args }) => {
                // The mir guarantees the callee resolves to exactly one
                // Function-typed value -- there is no way to construct a
                // Function-typed expression other than a function place
                // root, which always yields a single address.
                let fnaddr = self.process_expr(builder, *callee)[0];

                let fixed_count = fn_type.params.len();
                let mut ir_args = vec![];
                for (i, arg) in args.into_iter().enumerate() {
                    let arg_type = arg.r#type.clone();
                    let mut value = self.process_expr(builder, arg);
                    // Only the variadic tail needs default-argument
                    // promotion; a fixed/named parameter's width is already
                    // pinned by the callee's declared signature.
                    if fn_type.is_variadic && i >= fixed_count && let [v] = value.as_mut_slice() {
                        *v = self.promote_variadic_arg(builder, *v, &arg_type);
                    }
                    ir_args.push(value);
                }
                let mut ir_args = ir_args.into_iter().flatten().collect::<Vec<_>>();

                // A large return value needs caller-provided memory: an
                // anonymous slot whose address rides in as the hidden
                // leading StructReturn argument (mirroring
                // `make_function_sig`/`define_function_def`); the value's
                // leaves are read back out of it after the call.
                let sret_slot = self.needs_sret(&fn_type.return_type).then(|| {
                    let shift = stack_align_shift(type_alignment(&fn_type.return_type));
                    let size = total_bytes((*fn_type.return_type).clone(), self);
                    let slot = builder
                        .create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, size, shift));
                    let pointer = builder.ins().stack_addr(self.pointer_type(), slot, 0);
                    ir_args.insert(0, pointer);
                    slot
                });

                let call = if fn_type.is_variadic {
                    // Cranelift does not currently support variadic functions.
                    // To bypass that, we previously set the call convention to SysV
                    // and we are now going to "cast" the function pointer according
                    // to which arguments are being passed after the pre-determined params.
                    let mut sig = self.make_function_sig(fn_type.clone());
                    if ir_args.len() > sig.params.len() {
                        for arg in &ir_args[sig.params.len()..] {
                            sig.params
                                .push(AbiParam::new(builder.func.dfg.value_type(*arg)))
                        }
                    }
                    let sigref = builder.import_signature(sig);
                    builder.ins().call_indirect(sigref, fnaddr, &ir_args)
                } else {
                    let sig = self.make_function_sig(fn_type.clone());
                    let sigref = builder.import_signature(sig);
                    builder.ins().call_indirect(sigref, fnaddr, &ir_args)
                };

                if *fn_type.return_type == ResolvedType::Void {
                    return vec![];
                }

                match sret_slot {
                    Some(slot) => {
                        let storage = PlaceStorage::Slot { slot, offset: 0 };
                        self.load_scalars(builder, &storage, &fn_type.return_type)
                    }
                    None => builder.inst_results(call).to_vec(),
                }
            }

            // `*Concrete` -> `spec *Spec`: builds the fat pointer's two
            // leaves -- `base`'s own value unchanged (the data pointer)
            // plus the address of a lazily-built, memoized vtable (see
            // `vtable_for`). `node.r#type` is always `SpecObject` here
            // (`Analyzer::coerce_to_expected` guarantees it); `base`'s own
            // type is always a plain `Pointer` to the concrete struct/enum/
            // union that vtable is built for.
            MirExpr::SpecCoerce(MirSpecCoerce { base }) => {
                let ResolvedType::SpecObject { spec, .. } = &node.r#type else {
                    unreachable!("mir body guarantees a SpecCoerce's own type is SpecObject");
                };
                let spec = spec.clone();
                let ResolvedType::Pointer { pointee, .. } = &base.r#type else {
                    unreachable!("mir body guarantees a SpecCoerce's base is a plain pointer");
                };
                let concrete = (**pointee).clone();
                let data_ptr = self.process_expr(builder, *base)[0];
                let vtable_id = self.vtable_for(&concrete, &spec);
                let global_value = self.module.declare_data_in_func(vtable_id, builder.func);
                let vtable_ptr = builder.ins().global_value(self.pointer_type(), global_value);
                vec![data_ptr, vtable_ptr]
            }

            // `base.method(args)` through a `spec *Spec` value -- loads the
            // function pointer out of `base`'s own vtable leaf at
            // `slot_index * pointer_width` and calls through it, reusing
            // the exact same `call_indirect`/`make_function_sig` path
            // every ordinary call already goes through; only how the
            // callee address itself is obtained differs (a vtable load
            // here, `func_addr`/`get_place_value` for an ordinary call).
            // `self` is `base`'s own data-pointer leaf, prepended exactly
            // like an ordinary method call's own implicit self.
            MirExpr::DynamicCall(MirDynamicCall { base, slot_index, fn_type, args }) => {
                let base_leaves = self.get_place_value(&base, builder);
                let [data_ptr, vtable_ptr] = base_leaves.as_slice() else {
                    panic!("mir body guarantees a SpecObject place has exactly 2 leaves");
                };
                let (data_ptr, vtable_ptr) = (*data_ptr, *vtable_ptr);

                let ptr_bytes = self.pointer_type().bytes();
                let slot_offset = slot_index as i32 * ptr_bytes as i32;
                let fnaddr = builder.ins().load(self.pointer_type(), MemFlags::new(), vtable_ptr, slot_offset);

                let mut ir_args = vec![data_ptr];
                for arg in args {
                    ir_args.extend(self.process_expr(builder, arg));
                }

                let sret_slot = self.needs_sret(&fn_type.return_type).then(|| {
                    let shift = stack_align_shift(type_alignment(&fn_type.return_type));
                    let size = total_bytes((*fn_type.return_type).clone(), self);
                    let slot = builder
                        .create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, size, shift));
                    let pointer = builder.ins().stack_addr(self.pointer_type(), slot, 0);
                    ir_args.insert(0, pointer);
                    slot
                });

                let sig = self.make_function_sig(fn_type.clone());
                let sigref = builder.import_signature(sig);
                let call = builder.ins().call_indirect(sigref, fnaddr, &ir_args);

                if *fn_type.return_type == ResolvedType::Void {
                    return vec![];
                }
                match sret_slot {
                    Some(slot) => {
                        let storage = PlaceStorage::Slot { slot, offset: 0 };
                        self.load_scalars(builder, &storage, &fn_type.return_type)
                    }
                    None => builder.inst_results(call).to_vec(),
                }
            }

            MirExpr::Number(value) => {
                // The one and only leaf of `node.r#type`'s own flattening --
                // every resolved numeric type is exactly one IR leaf --
                // picks the concrete width/kind to narrow `value` into.
                // `value` itself is already range-checked against this same
                // type by analysis, so this never has to reject anything,
                // only narrow losslessly.
                let ir_type = node.r#type.clone().into_ir_type(self)[0];
                let result = match value {
                    NumberValue::Signed(v) => builder.ins().iconst(ir_type, v),
                    NumberValue::Unsigned(v) => builder.ins().iconst(ir_type, v as i64),
                    NumberValue::Float(v) if ir_type == types::F32 => builder.ins().f32const(v as f32),
                    NumberValue::Float(v) => builder.ins().f64const(v),
                };
                vec![result]
            }

            MirExpr::Bool(b) => vec![builder.ins().iconst(types::I8, b as i64)],

            // `sizeof<Type>` -- a compile-time-known `usize` constant.
            // Fully general (unlike `sizeof<Type>` used *inside* an
            // `@layout` argument, which is scoped to primitives -- see
            // `ResolvedType::primitive_byte_size`'s doc comment): `Type`
            // may be any struct/enum/primitive, since `total_bytes` already
            // handles all of them uniformly.
            MirExpr::Sizeof(target_type) => {
                let size = total_bytes(target_type, self);
                vec![builder.ins().iconst(self.pointer_type(), size as i64)]
            }

            // Cranelift has no dedicated char/codepoint type -- a `char`'s
            // one IR leaf is just its `u32` codepoint stored in an `I32`
            // (see `Char`'s `into_ir_type` arm).
            MirExpr::Char(c) => vec![builder.ins().iconst(types::I32, c as i64)],

            MirExpr::Place(place) => self.get_place_value(&place, builder),

            MirExpr::Assignment(MirAssignment { target, value }) => {
                let values = self.process_expr(builder, *value);
                // Uniformly covers assignment to a local, through any depth
                // of explicit/seamless deref (`*ptr = 5;`, `ptr.field = 5;`),
                // and through array indexing -- whatever `target` resolved
                // to, `store_scalars` only cares whether it has an address
                // (`todo!()`s itself for the one case that doesn't yet, a
                // parameter with no deref in between).
                let (storage, _) = self.resolve_place_storage(&target, builder);
                self.store_scalars(builder, &storage, &values);
                values
            }

            MirExpr::AddressOf(MirAddressOf { place }) => {
                let (storage, _) = self.resolve_place_storage(&place, builder);
                vec![self.place_storage_address(builder, &storage)]
            }

            MirExpr::Negate(base) => {
                // The mir guarantees only signed ints or floats reach here
                // -- `fneg` for the latter, `ineg` (two's-complement
                // negation) for the former.
                let is_float = matches!(base.r#type.numeric_kind(), Some(NumericKind::Float(_)));
                let value = self.process_expr(builder, *base)[0];
                let result = if is_float { builder.ins().fneg(value) } else { builder.ins().ineg(value) };
                vec![result]
            }

            MirExpr::BitNot(base) => {
                // The mir guarantees only signed/unsigned integers reach
                // here.
                let value = self.process_expr(builder, *base)[0];
                vec![builder.ins().bnot(value)]
            }

            MirExpr::BinaryOp(MirBinaryOp { op, left, right }) => {
                // The mir guarantees both operands share the same resolved
                // type, so either one's `numeric_kind` picks the right
                // instruction for the whole operation. `Char` is the one
                // exception: it has no `numeric_kind` of its own (see its
                // doc comment -- arithmetic/bitwise on it is meaningless,
                // possibly UTF-8-breaking), but analysis only ever lets it
                // reach here for a *comparison* op, where it behaves
                // exactly like its underlying representation: an unsigned
                // 4-byte scalar, ordered by codepoint.
                let kind = match &left.r#type {
                    ResolvedType::Char => NumericKind::Unsigned(32),
                    r#type => r#type
                        .numeric_kind()
                        .expect("mir body guarantees BinaryOp operands are numeric or char"),
                };
                let left = self.process_expr(builder, *left)[0];
                let right = self.process_expr(builder, *right)[0];
                // Division/modulo by zero traps at the instruction level --
                // consistent with this language having no other runtime
                // safety net (no bounds checks either), so no special
                // handling is needed here.
                let result = match (op, kind) {
                    (BinaryOp::Add, NumericKind::Float(_)) => builder.ins().fadd(left, right),
                    (BinaryOp::Add, _) => builder.ins().iadd(left, right),
                    (BinaryOp::Sub, NumericKind::Float(_)) => builder.ins().fsub(left, right),
                    (BinaryOp::Sub, _) => builder.ins().isub(left, right),
                    (BinaryOp::Mul, NumericKind::Float(_)) => builder.ins().fmul(left, right),
                    (BinaryOp::Mul, _) => builder.ins().imul(left, right),
                    (BinaryOp::Div, NumericKind::Float(_)) => builder.ins().fdiv(left, right),
                    (BinaryOp::Div, NumericKind::Signed(_)) => builder.ins().sdiv(left, right),
                    (BinaryOp::Div, NumericKind::Unsigned(_)) => builder.ins().udiv(left, right),
                    (BinaryOp::Rem, NumericKind::Signed(_)) => builder.ins().srem(left, right),
                    (BinaryOp::Rem, NumericKind::Unsigned(_)) => builder.ins().urem(left, right),
                    (BinaryOp::Rem, NumericKind::Float(_)) => {
                        unreachable!("mir body rejects '%' on float operands")
                    }
                    // The mir guarantees neither operand is a float for any
                    // of these -- signedness never matters except for
                    // `>>`, which needs to pick arithmetic (sign-extending)
                    // vs. logical shift.
                    (BinaryOp::BitAnd, _) => builder.ins().band(left, right),
                    (BinaryOp::BitOr, _) => builder.ins().bor(left, right),
                    (BinaryOp::BitXor, _) => builder.ins().bxor(left, right),
                    (BinaryOp::Shl, _) => builder.ins().ishl(left, right),
                    (BinaryOp::Shr, NumericKind::Signed(_)) => builder.ins().sshr(left, right),
                    (BinaryOp::Shr, NumericKind::Unsigned(_)) => builder.ins().ushr(left, right),
                    (BinaryOp::Shr, NumericKind::Float(_)) => {
                        unreachable!("mir body rejects '>>' on float operands")
                    }
                    (cmp, NumericKind::Float(_)) => {
                        let cc = match cmp {
                            BinaryOp::Eq => FloatCC::Equal,
                            BinaryOp::Ne => FloatCC::NotEqual,
                            BinaryOp::Lt => FloatCC::LessThan,
                            BinaryOp::Le => FloatCC::LessThanOrEqual,
                            BinaryOp::Gt => FloatCC::GreaterThan,
                            BinaryOp::Ge => FloatCC::GreaterThanOrEqual,
                            _ => unreachable!("not a comparison op"),
                        };
                        builder.ins().fcmp(cc, left, right)
                    }
                    (cmp, NumericKind::Signed(_)) => {
                        let cc = match cmp {
                            BinaryOp::Eq => IntCC::Equal,
                            BinaryOp::Ne => IntCC::NotEqual,
                            BinaryOp::Lt => IntCC::SignedLessThan,
                            BinaryOp::Le => IntCC::SignedLessThanOrEqual,
                            BinaryOp::Gt => IntCC::SignedGreaterThan,
                            BinaryOp::Ge => IntCC::SignedGreaterThanOrEqual,
                            _ => unreachable!("not a comparison op"),
                        };
                        builder.ins().icmp(cc, left, right)
                    }
                    (cmp, NumericKind::Unsigned(_)) => {
                        let cc = match cmp {
                            BinaryOp::Eq => IntCC::Equal,
                            BinaryOp::Ne => IntCC::NotEqual,
                            BinaryOp::Lt => IntCC::UnsignedLessThan,
                            BinaryOp::Le => IntCC::UnsignedLessThanOrEqual,
                            BinaryOp::Gt => IntCC::UnsignedGreaterThan,
                            BinaryOp::Ge => IntCC::UnsignedGreaterThanOrEqual,
                            _ => unreachable!("not a comparison op"),
                        };
                        builder.ins().icmp(cc, left, right)
                    }
                };
                vec![result]
            }

            MirExpr::ArrayLiteral(MirArrayLiteral { elements, .. }) => {
                // Each element contributes its own leaves, in order -- the
                // exact flattening `ResolvedType::SizedArray`'s `into_ir_type`
                // expects, so the result is usable anywhere a `SizedArray`
                // value already is (assignment, a walrus's inferred value, ...).
                elements.into_iter().flat_map(|e| self.process_expr(builder, e)).collect()
            }

            MirExpr::EnumConstruct(MirEnumConstruct { variant_index, fields }) => {
                // Built in an anonymous scratch slot -- constants (tag,
                // header), the shared dynamic fields, and typed body fields
                // all land at their byte offsets, the rest of the payload
                // region is zeroed (deterministic bytes for the chunk-wise
                // copies the leaf model does; the dynamic-fields region
                // needs no such zeroing -- every dynamic field is always
                // supplied by `fields` below, unlike the payload's union
                // slack) -- then the whole value is read back out as
                // ordinary leaves.
                let ResolvedType::Enum { cell, .. } = &node.r#type else {
                    unreachable!("mir body guarantees a construction's own type is its enum");
                };
                let cell = cell.clone();
                // Snapshot everything needed so the cell isn't borrowed
                // across the field-value evaluation below.
                let (tag, tag_type, header, payload_offset, chunks, field_offsets) = {
                    let enum_type = cell.borrow();
                    let variant = &enum_type.variants[variant_index];
                    let header: Vec<(ResolvedType, ConstValue)> = enum_type
                        .header
                        .iter()
                        .zip(&variant.header_values)
                        .map(|((_, r#type, _), value)| (r#type.clone(), value.clone()))
                        .collect();
                    // `field.field_index` (from `MirEnumConstruct::fields`)
                    // spans the *combined* declared list analysis built --
                    // shared dynamic fields first, then this variant's own
                    // body fields -- so this offset table is built in that
                    // exact same order.
                    let field_offsets: Vec<u32> = (0..enum_type.dynamic_fields.len())
                        .map(|i| enum_dynamic_field_offset(&enum_type, i, self))
                        .chain(
                            (0..variant.fields.len())
                                .map(|i| enum_body_field_offset(&enum_type, variant_index, i, self)),
                        )
                        .collect();
                    (
                        variant.tag,
                        enum_type.tag_type.clone(),
                        header,
                        enum_payload_offset(&enum_type, self),
                        payload_chunks(enum_payload_bytes(&enum_type, enum_type.layout.pack, self)),
                        field_offsets,
                    )
                };

                let shift = stack_align_shift(type_alignment(&node.r#type));
                let total = total_bytes(node.r#type.clone(), self);
                let slot = builder
                    .create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, total, shift));

                let tag_values = self.emit_const_value(builder, &ConstValue::Number(tag), &tag_type);
                self.store_scalars(builder, &PlaceStorage::Slot { slot, offset: 0 }, &tag_values);

                let mut offset = total_bytes(tag_type, self);
                for (r#type, value) in &header {
                    let const_values = self.emit_const_value(builder, value, r#type);
                    self.store_scalars(builder, &PlaceStorage::Slot { slot, offset }, &const_values);
                    offset += total_bytes(r#type.clone(), self);
                }

                let mut chunk_offset = payload_offset;
                for chunk in chunks {
                    let zero = builder.ins().iconst(chunk, 0);
                    builder.ins().stack_store(zero, slot, chunk_offset as i32);
                    chunk_offset += chunk.bytes();
                }

                // Dynamic and body field values run in source order (their
                // side effects must); each lands at its declared field's
                // offset.
                for field in fields {
                    let field_offset = field_offsets[field.field_index];
                    let values = self.process_expr(builder, field.value);
                    self.store_scalars(builder, &PlaceStorage::Slot { slot, offset: field_offset }, &values);
                }

                self.load_scalars(builder, &PlaceStorage::Slot { slot, offset: 0 }, &node.r#type)
            }

            MirExpr::StructLiteral(MirStructLiteral { fields }) => {
                // Values are evaluated in the order the user wrote them
                // (their side effects must run in source order), but the
                // result's leaves are concatenated in *declared field*
                // order -- the exact flattening `ResolvedType::Struct`'s
                // `into_ir_type` expects, so the result is usable anywhere
                // a struct value already is. The mir guarantees every
                // declared field appears exactly once.
                let ResolvedType::Struct(struct_type) = &node.r#type else {
                    unreachable!("mir body guarantees a struct literal's own type is a struct");
                };
                let field_count = struct_type.borrow().fields.len();
                let mut per_field: Vec<Option<Vec<Value>>> = vec![None; field_count];
                for field in fields {
                    per_field[field.field_index] = Some(self.process_expr(builder, field.value));
                }
                per_field
                    .into_iter()
                    .map(|leaves| leaves.expect("mir body guarantees every field is initialized"))
                    .flatten()
                    .collect()
            }

            MirExpr::Slice(MirSlice { base, item_type, start, end, inclusive }) => {
                let (storage, base_type) = self.resolve_place_storage(&base, builder);
                let ptr_type = self.pointer_type();

                // A slice's data pointer and full length, however `base` is
                // actually stored: a `SizedArray`'s elements live inline, so
                // the pointer is the storage's own address and the length is
                // a compile-time constant; a `Slice`/`Str` already carries
                // both as its two flattened leaves (identical layout for
                // both -- re-slicing a `*str` produces another `*str`,
                // decided by `node.r#type` above this match, not by
                // anything read here).
                let (data_ptr, full_len) = match &base_type {
                    ResolvedType::SizedArray(_, size) => {
                        let ptr = self.place_storage_address(builder, &storage);
                        let len = builder.ins().iconst(types::I32, *size as i64);
                        (ptr, len)
                    }
                    ResolvedType::Slice { .. } | ResolvedType::Str { .. } => {
                        let leaves = self.load_scalars(builder, &storage, &base_type);
                        (leaves[0], leaves[1])
                    }
                    _ => unreachable!("mir body guarantees a slice's base is SizedArray/Slice/Str"),
                };

                let elem_size = total_bytes(item_type, self) as i64;

                let start_val = match start {
                    Some(e) => self.process_expr(builder, *e)[0],
                    None => builder.ins().iconst(types::I32, 0),
                };
                // An inclusive end (`...`) with an explicit bound includes
                // that element itself, so it's one past `end` in the
                // exclusive terms the rest of this function computes in; an
                // absent end always means "through the real end of `base`"
                // regardless of `inclusive` -- there's nothing to be
                // exclusive *of* when there's no bound at all.
                let end_val = match end {
                    Some(e) => {
                        let v = self.process_expr(builder, *e)[0];
                        if inclusive { builder.ins().iadd_imm(v, 1) } else { v }
                    }
                    None => full_len,
                };

                let mut start_ext = start_val;
                if builder.func.dfg.value_type(start_ext) != ptr_type {
                    start_ext = builder.ins().uextend(ptr_type, start_ext);
                }
                let elem_size_val = builder.ins().iconst(ptr_type, elem_size);
                let byte_offset = builder.ins().imul(start_ext, elem_size_val);
                let new_ptr = builder.ins().iadd(data_ptr, byte_offset);
                let new_len = builder.ins().isub(end_val, start_val);

                vec![new_ptr, new_len]
            }

            MirExpr::Cast(MirCast { kind, target_type, base }) => {
                // Captures every leaf, not just the first -- `Reinterpret`
                // needs all of them (a fat pointer's `[ptr, len]` passed
                // through unchanged, same as a numeric cast's own single
                // leaf passed through unchanged); every other `CastKind`
                // only ever applies to a single-leaf numeric source (`Str`/
                // `Slice` never reach them -- `cast_class` stays `None` for
                // both, so `byte_pointer_cast_kind` always intercepts
                // first), so indexing `[0]` for those is still exactly
                // right.
                let base_leaves = self.process_expr(builder, *base);
                let target_ir = target_type.into_ir_type(self)[0];
                match kind {
                    CastKind::Reinterpret => base_leaves,
                    CastKind::DropLength => vec![base_leaves[0]],
                    CastKind::IntExtend { signed: true } => vec![builder.ins().sextend(target_ir, base_leaves[0])],
                    CastKind::IntExtend { signed: false } => vec![builder.ins().uextend(target_ir, base_leaves[0])],
                    CastKind::IntTruncate => vec![builder.ins().ireduce(target_ir, base_leaves[0])],
                    CastKind::IntToFloat { signed: true } => vec![builder.ins().fcvt_from_sint(target_ir, base_leaves[0])],
                    CastKind::IntToFloat { signed: false } => vec![builder.ins().fcvt_from_uint(target_ir, base_leaves[0])],
                    CastKind::FloatToInt { signed: true } => vec![builder.ins().fcvt_to_sint_sat(target_ir, base_leaves[0])],
                    CastKind::FloatToInt { signed: false } => vec![builder.ins().fcvt_to_uint_sat(target_ir, base_leaves[0])],
                    CastKind::FloatExtend => vec![builder.ins().fpromote(target_ir, base_leaves[0])],
                    CastKind::FloatTruncate => vec![builder.ins().fdemote(target_ir, base_leaves[0])],
                }
            }

            MirExpr::UnionConstruct(MirUnionConstruct { field_index: _, value }) => {
                // Mirrors `EnumConstruct`'s shape (anonymous slot, zero the
                // whole region deterministically, store the one field's
                // scalars, read the whole thing back as flattened leaves) --
                // minus the tag/header steps, since a union has neither.
                let total = total_bytes(node.r#type.clone(), self);
                let slot = builder
                    .create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, total, 4));

                let mut chunk_offset = 0u32;
                for chunk in node.r#type.clone().into_ir_type(self) {
                    let zero = builder.ins().iconst(chunk, 0);
                    builder.ins().stack_store(zero, slot, chunk_offset as i32);
                    chunk_offset += chunk.bytes();
                }

                let values = self.process_expr(builder, *value);
                self.store_scalars(builder, &PlaceStorage::Slot { slot, offset: 0 }, &values);

                self.load_scalars(builder, &PlaceStorage::Slot { slot, offset: 0 }, &node.r#type)
            }
        }
    }

    /// A function/method's cranelift `Signature`, built the same way
    /// regardless of whether it's being declared (pass 1) or defined (pass
    /// 2) -- and, crucially, the same way *call sites* build it: one
    /// delegation to `make_function_sig`, so the definition and every call
    /// can never disagree about parameter flattening or the hidden
    /// struct-return pointer.
    fn function_signature(&self, function_def: &MirFunctionDef) -> Signature {
        self.make_function_sig(function_def.fn_type())
    }

    /// Declares (but doesn't yet define the body of) a function or method --
    /// signature/symbol registration only, split out from what used to be
    /// one `update_function_def` specifically so *every* function across
    /// *every* compiled module can be declared (and therefore have a
    /// `FuncId` any other module's body can already look up) before *any*
    /// body starts being built. Without this split, a cross-module call in
    /// either import direction would panic the first time one module's body
    /// needed another module's not-yet-declared `FuncId`.
    ///
    /// `linkage` is `Linkage::Export` (strong) for an ordinary item and
    /// `Linkage::Preemptible` (weak) for a generic instantiation -- see
    /// `declare_item`'s `linkage_for`, this function's only caller for the
    /// choice. A within-process collision between two *different* strong
    /// symbols is still exactly the `@mangling(disabled)` user error this
    /// check has always caught; it's untouched by generics, since the
    /// driver's own `ItemKey` cache already guarantees at most one
    /// `MirFunctionDef` per instantiation reaches this function at all
    /// within a single compilation -- weak linkage is what lets two
    /// *separate* compilations' independently-generated copies fold into
    /// one at link time, a scenario this in-process map never sees.
    fn declare_function_def(&mut self, function_def: &MirFunctionDef, symbol: String, linkage: Linkage) -> FuncId {
        let sig = self.function_signature(function_def);

        if let Some(&existing_id) = self.declared_symbols.get(&symbol)
            && existing_id != function_def.id
        {
            self.symbol_error.get_or_insert_with(|| {
                format!(
                    "two different functions both produce the linker symbol '{symbol}' -- this can \
                     happen when '@mangling(disabled)' is used on more than one function with the same name; \
                     give one of them a different name, or re-enable mangling"
                )
            });
            // Don't ask `cranelift_module` to declare a second `Export` for
            // a symbol it already has one for -- it rejects that outright
            // (a real, deliberate safety check on its part), which would
            // panic here instead of surfacing `symbol_error` cleanly. The
            // object file is discarded either way once `symbol_error` is
            // set (see `Codegen::generate`), so reusing the first
            // definition's `FuncId` for this one is harmless -- it only
            // has to survive long enough to let this pass finish.
            let existing_function_id =
                *self.functions.get(&existing_id).expect("a declared symbol's owner is always already in `functions`");
            self.functions.insert(function_def.id, existing_function_id);
            return existing_function_id;
        }
        self.declared_symbols.insert(symbol.clone(), function_def.id);

        let function_id = self.module.declare_function(&symbol, Linkage::Import, &sig).unwrap();

        self.module.declare_function(&symbol, linkage, &sig).unwrap();

        self.functions.insert(function_def.id, function_id);
        function_id
    }

    /// `declare_function_def`'s extern-module counterpart: declares a link
    /// against an extern-owned function/method, but `Linkage::Import` only
    /// -- no paired `Export` declare, and `define_item`'s pass 2 never sees
    /// this `HirId` at all (it isn't in any `MirModule.items`), so no body
    /// is ever generated for it here. `extern_fn.mangling` (resolved by the
    /// *declaring* compilation, at signature time -- see
    /// `omega_analyzer::annotations`' doc comment and `ExternFunctionRef::
    /// mangling`'s own) decides which symbol-shape branch below applies,
    /// mirroring `declare_item`'s identical branch for a local function:
    /// whatever that other `omgc` invocation actually mangled this
    /// declaration as is exactly what gets linked against here, never
    /// assumed. Trusts that the *other* `omgc` invocation compiling that
    /// module standalone mangles its own definition identically -- see
    /// `CompiledProgram::extern_functions`'s doc comment for why that's a
    /// safe assumption.
    fn declare_extern_function(&mut self, extern_fn: &ExternFunctionRef) {
        let mangled = match (extern_fn.mangling, &extern_fn.kind) {
            (ManglingMode::Disabled, ExternFunctionKind::Free(name)) => name.as_ref().to_string(),
            // `@mangling(disabled)` is rejected on methods at analysis time
            // -- an extern method's own declaration went through the exact
            // same check, so this combination can't actually occur.
            (ManglingMode::Disabled, ExternFunctionKind::Method { .. }) => {
                unreachable!("'@mangling(disabled)' is rejected on methods at analysis time")
            }
            // `collect_extern_functions` only ever surfaces non-generic
            // extern items (a generic reached through `--extern` is always
            // fully recompiled locally instead), so there's no owner/free
            // generic-args data to pass here -- always `&[]`.
            (ManglingMode::Enabled, ExternFunctionKind::Free(name)) => {
                mangle::encode(&mangle::free_function_symbol(&extern_fn.module_path, name, &[], &extern_fn.fn_type))
            }
            (ManglingMode::Enabled, ExternFunctionKind::Method { type_name, method_name }) => mangle::encode(
                &mangle::method_symbol(&extern_fn.module_path, type_name, &[], method_name, &extern_fn.fn_type),
            ),
        };
        let sig = self.make_function_sig(extern_fn.fn_type.clone());

        let function_id = self.module.declare_function(&mangled, Linkage::Import, &sig).unwrap();
        self.functions.insert(extern_fn.decl_id, function_id);
    }

    /// Builds a function/method's body -- everything `update_function_def`
    /// used to do after declaring, now looking up the `FuncId` every item
    /// across every module already got in the declare pass, rather than
    /// declaring (and re-registering) it itself.
    ///
    /// Unlike the pre-mir version of this function, there is no
    /// `BlockOutcome`/`return_block`/`loop_stack`/`defer_flags` bookkeeping
    /// here at all -- the mir body already *is* the control-flow graph
    /// (every `if`/`match`/`while`/`for`/`break`/`continue`/`return`/
    /// `defer` was flattened into it during lowering, see
    /// `omega-docs/16-mir-and-codegen.md`), so this is just: one Cranelift
    /// `Block` per `MirBlockData`, then translate each one's statements and
    /// its single terminator.
    fn define_function_def(&mut self, function_def: MirFunctionDef) {
        // A symbol collision (see `declare_function_def`) is always found
        // during the declare pass, which fully finishes before any define
        // pass starts (see `update_all`'s doc comment) -- so once one's
        // been found, every remaining body is skipped outright rather than
        // defined against whatever `FuncId` `declare_function_def` had to
        // improvise for the colliding function (which would otherwise ask
        // `cranelift_module` to define the same `FuncId` twice and panic).
        // `Codegen::generate` discards this whole `Codegen` once
        // `symbol_error` is `Some`, so an incomplete module here is fine.
        if self.symbol_error.is_some() {
            return;
        }

        let function_id = *self
            .functions
            .get(&function_def.id)
            .expect("declared for every item, across every module, before any body is defined");
        let sig = self.function_signature(&function_def);
        let MirFunctionDef { return_type, body, .. } = function_def;

        // Move `ctx` out of `self` for the duration of the build so the rest of
        // this function can still freely borrow `self` (e.g. `into_ir_type(&self)`,
        // `self.process_expr(...)`) while `builder` holds onto it.
        let mut ctx = std::mem::replace(&mut self.ctx, codegen::Context::new());
        // `ctx.clear()` (called for every function via `clear_local`, below)
        // resets `want_disasm` back to `false` each time, so this has to be
        // set again per function, not once up front.
        ctx.set_disasm(self.emit == EmitKind::Asm);
        let mut fbctx = FunctionBuilderContext::new();
        let mut builder = FunctionBuilder::new(&mut ctx.func, &mut fbctx);
        builder.func.signature = sig;

        self.arg_count = body.arg_count;
        self.stack_slots = vec![None; body.locals.len()];
        self.local_args = vec![Vec::new(); body.locals.len()];

        // One cranelift block per mir block, minted up front so a forward
        // reference (or a loop's own back-edge) always resolves regardless
        // of which order the blocks below get filled in.
        let cranelift_blocks: Vec<Block> = body.blocks.iter().map(|_| builder.create_block()).collect();
        let entry_block = cranelift_blocks[0];

        builder.append_block_params_for_function_params(entry_block);
        let block_params = builder.block_params(entry_block).to_vec();

        // A large return value comes back through a hidden StructReturn
        // pointer, always the signature's first parameter (see
        // `make_function_sig`) -- peel it off before mapping the *declared*
        // parameters below.
        let sret = self.needs_sret(&return_type).then(|| block_params[0]);
        let declared_params = &block_params[sret.is_some() as usize..];

        // Some identifiers (e.g structs) have more than one value per identifier.
        // For that reason, lets make a helper array that repeats the local's own
        // index N times, where N is the amount of values it has.
        let argmap: Vec<usize> = body.locals[..body.arg_count]
            .iter()
            .enumerate()
            .flat_map(|(i, local)| {
                let value_count = local.r#type.clone().into_ir_type(self).len();
                vec![i; value_count]
            })
            .collect();
        for (i, arg) in declared_params.iter().enumerate() {
            self.local_args[argmap[i]].push(*arg);
        }

        for (mir_block, &cranelift_block) in body.blocks.into_iter().zip(&cranelift_blocks) {
            builder.switch_to_block(cranelift_block);
            for stmt in mir_block.statements {
                self.process_expr(&mut builder, stmt);
            }
            match mir_block.terminator {
                MirTerminator::Goto(target) => {
                    builder.ins().jump(cranelift_blocks[target.0 as usize], &[]);
                }
                MirTerminator::Branch { condition, then_block, else_block } => {
                    let cond_value = self.process_expr(&mut builder, condition)[0];
                    builder.ins().brif(
                        cond_value,
                        cranelift_blocks[then_block.0 as usize],
                        &[],
                        cranelift_blocks[else_block.0 as usize],
                        &[],
                    );
                }
                MirTerminator::Return(value) => {
                    let leaves = value.map(|v| self.process_expr(&mut builder, v)).unwrap_or_default();
                    // With a StructReturn pointer, the value leaves are
                    // stored through it and the signature declares no
                    // return values (cranelift itself returns the pointer
                    // in rax per the SysV rule); otherwise the leaves
                    // return in registers as before.
                    match sret {
                        Some(pointer) => {
                            self.store_scalars(
                                &mut builder,
                                &PlaceStorage::Address { base: pointer, offset: 0 },
                                &leaves,
                            );
                            builder.ins().return_(&[]);
                        }
                        None => {
                            builder.ins().return_(&leaves);
                        }
                    }
                }
                MirTerminator::Unreachable => {
                    builder.ins().trap(TrapCode::unwrap_user(1));
                }
            }
        }

        // Every cranelift block was created up front and every terminator
        // above has now been emitted, so every block's predecessor set is
        // already final -- safe to seal all of them in one pass here,
        // rather than interleaved with the loop above.
        for block in cranelift_blocks {
            builder.seal_block(block);
        }

        if let Err(err) = codegen::verify_function(builder.func, self.isa.as_ref()) {
            panic!("cranelift verifier rejected generated IR for a function (internal codegen bug): {err:?}");
        }

        builder.finalize();

        self.module.define_function(function_id, &mut ctx).unwrap();

        // `ctx.func` (the CLIF) and `ctx.compiled_code()` (populated by the
        // `define_function` call just above, since `set_disasm` was set
        // on this same `ctx` above) are both still valid to read here --
        // `define_function` fills in the compile *result*, it doesn't
        // consume the IR that produced it.
        match self.emit {
            EmitKind::Ir => {
                let name = self.module.declarations().get_function_decl(function_id).name.clone().unwrap_or_default();
                self.captured_text.push_str(&format!("; {name}\n{}\n\n", ctx.func));
            }
            EmitKind::Asm => {
                let name = self.module.declarations().get_function_decl(function_id).name.clone().unwrap_or_default();
                let vcode = ctx.compiled_code().and_then(|c| c.vcode.clone()).unwrap_or_default();
                self.captured_text.push_str(&format!("; {name}\n{vcode}\n\n"));
            }
            EmitKind::Obj => {}
        }

        self.ctx = ctx;

        self.clear_local();
    }

    /// `Linkage::Export` (strong) for an ordinary item, `Linkage::
    /// Preemptible` (weak) for a generic instantiation -- `Preemptible`
    /// maps to a genuine weak ELF/Mach-O/COFF symbol (`cranelift-object`'s
    /// `translate_linkage`, `let weak = linkage == Linkage::Preemptible`),
    /// empirically confirmed to let a linker silently fold multiple
    /// independently-compiled definitions of the *same* symbol name into
    /// one, rather than erroring on "multiple definition" the way two
    /// strong symbols with the same name always would. Every separate
    /// `omgc` invocation that instantiates e.g. `CustomStruct<i32>` still
    /// fully regenerates its own copy locally (nothing here skips that --
    /// there is no cross-process build cache), exactly like Rust/C++
    /// generics: the deduplication happens once, at final link time, not
    /// at compile time. This is only sound because a generic
    /// instantiation's mangled symbol is a pure function of `(module_path,
    /// name, type_args)` -- two independent compilations of the exact same
    /// instantiation are therefore guaranteed to produce byte-identical
    /// bodies under the identical name, which is the actual precondition
    /// weak-symbol folding relies on (the linker trusts the name, it
    /// doesn't diff the bytes). An ordinary, non-generic symbol keeps
    /// strong linkage unconditionally -- two *different* object files
    /// defining the same non-generic symbol is always a genuine user
    /// error, and should still be a hard link error, not silently
    /// tolerated.
    fn linkage_for(type_args: &[ResolvedType]) -> Linkage {
        if type_args.is_empty() { Linkage::Export } else { Linkage::Preemptible }
    }

    /// Declares every function/method/extern in one item -- pass 1 of 2 (see
    /// `update_all`).
    fn declare_item(&mut self, item: &MirItem, path: &[Ident], entry: &[Ident]) {
        match item {
            // Externs have no body to split across two passes -- fully
            // handled here, in one shot.
            MirItem::ExternDeclaration(extern_decl) => self.update_extern_decl(extern_decl.clone()),
            MirItem::FunctionDefinition(f) => {
                // A member function can never reach `Disabled` here --
                // `omega_analyzer::annotations::resolve` hard-rejects
                // `@mangling(disabled)` on a method (and on a generic
                // function) before a `CheckedModule` (and therefore the
                // `MirModule` lowered from it) can exist at all, so only a
                // top-level, non-generic function ever gets here with
                // `Disabled`.
                //
                // The program's literal entry point (`main`, in the entry
                // module) keeps the bare, unmangled symbol the OS/linker
                // looks for -- checked here, before a `Symbol` is even
                // built, rather than inside `mangle::free_function_symbol`,
                // which only ever needs to know how to name a real symbol.
                // `main` is never itself generic, so `linkage_for` already
                // gives it `Export`, same as today, with no special case
                // needed beyond the name.
                // `extension_target` -- `Some` for a method attached via
                // `spec Name : Deps for Target { ... }` (see
                // `MirFunctionDef::extension_target`'s doc comment) --
                // mangles like a struct/enum/union method (owned, via
                // `method_symbol`) rather than an ordinary free function:
                // the target's own `Display` stands in for the owner name a
                // primitive doesn't otherwise have, avoiding a collision
                // with an unrelated, same-named, same-`type_args`-shaped
                // free function elsewhere in the same module.
                let mangled = match (f.mangling, &f.extension_target) {
                    (ManglingMode::Disabled, _) => f.name.as_ref().to_string(),
                    (ManglingMode::Enabled, _) if path == entry && f.name.as_ref() == "main" => "main".to_string(),
                    (ManglingMode::Enabled, Some(target)) => {
                        let owner = Ident(target.to_string());
                        mangle::encode(&mangle::method_symbol(path, &owner, &[], &f.name, &f.fn_type()))
                    }
                    (ManglingMode::Enabled, None) => {
                        mangle::encode(&mangle::free_function_symbol(path, &f.name, &f.type_args, &f.fn_type()))
                    }
                };
                self.declare_function_def(f, mangled, Self::linkage_for(&f.type_args));
            }
            MirItem::Struct(s) => {
                for f in &s.functions {
                    let mangled =
                        mangle::encode(&mangle::method_symbol(path, &s.name, &s.type_args, &f.name, &f.fn_type()));
                    self.declare_function_def(f, mangled, Self::linkage_for(&s.type_args));
                }
            }
            MirItem::Enum(e) => {
                for f in &e.functions {
                    let mangled =
                        mangle::encode(&mangle::method_symbol(path, &e.name, &e.type_args, &f.name, &f.fn_type()));
                    self.declare_function_def(f, mangled, Self::linkage_for(&e.type_args));
                }
            }
            MirItem::Union(u) => {
                for f in &u.functions {
                    let mangled =
                        mangle::encode(&mangle::method_symbol(path, &u.name, &u.type_args, &f.name, &f.fn_type()));
                    self.declare_function_def(f, mangled, Self::linkage_for(&u.type_args));
                }
            }
            MirItem::Declaration(_) => {
                todo!("global data declarations are not yet implemented");
            }
        }
    }

    /// Defines every function/method body in one item -- pass 2 of 2, run
    /// only after every item across every module has already been declared.
    fn define_item(&mut self, item: MirItem) {
        match item {
            // Already fully handled by `declare_item` -- an extern has no
            // body to define.
            MirItem::ExternDeclaration(_) => {}
            MirItem::FunctionDefinition(f) => self.define_function_def(f),
            MirItem::Struct(s) => {
                for f in s.functions {
                    self.define_function_def(f);
                }
            }
            MirItem::Enum(e) => {
                for f in e.functions {
                    self.define_function_def(f);
                }
            }
            MirItem::Union(u) => {
                for f in u.functions {
                    self.define_function_def(f);
                }
            }
            MirItem::Declaration(_) => {
                todo!("global data declarations are not yet implemented");
            }
        }
    }

    /// Two full passes over every item across every compiled module: first
    /// declare everything (so any `FuncId` a cross-module call needs already
    /// exists, regardless of import direction -- see `declare_function_def`'s
    /// doc comment), then define every body. Mirrors the identical
    /// signature/body split `omega_analyzer::Analyzer` does for the same
    /// underlying reason.
    fn update_all(&mut self, modules: Vec<(Vec<Ident>, MirModule)>, entry: &[Ident], extern_functions: Vec<ExternFunctionRef>) {
        for (path, module) in &modules {
            for item in &module.items {
                self.declare_item(item, path, entry);
            }
        }
        for extern_fn in &extern_functions {
            self.declare_extern_function(extern_fn);
        }
        for (_, module) in modules {
            for item in module.items {
                self.define_item(item);
            }
        }
    }

    pub fn pointer_type(&self) -> IRType {
        self.module.target_config().pointer_type()
    }

    /// Produces whatever `emit` (passed to `generate`) asked for. `Obj`
    /// finishes and links the object exactly like the old `emit_object`
    /// did; `Ir`/`Asm` skip linking entirely -- nothing downstream needs
    /// the linked object in those modes, only the text `define_function_def`
    /// already accumulated into `captured_text` as each function compiled.
    pub fn finish(self) -> EmitOutput {
        match self.emit {
            EmitKind::Obj => EmitOutput::Object(self.module.finish().emit().unwrap()),
            EmitKind::Ir | EmitKind::Asm => EmitOutput::Text(self.captured_text),
        }
    }
}
