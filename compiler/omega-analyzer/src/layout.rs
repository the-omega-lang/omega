//! Backend-agnostic struct/enum/union layout math: byte offsets, padding,
//! and leaf-flattening for any [`ResolvedType`]. Every function here is
//! pure data computation over `ResolvedType` and a target's pointer width
//! -- nothing in this module names a specific backend's native IR type, so
//! a second backend (see `omega_codegen::BackendKind`) calls straight into
//! this instead of re-deriving struct/enum layout from scratch, and (the
//! reason this lives here, in `omega-analyzer`, rather than in
//! `omega-codegen` where it originated) a `comp` evaluation's own `sizeof`
//! support (`comp_eval::Interpreter`) can call straight into it too --
//! `omega-codegen` depends on `omega-analyzer`, never the other way, so
//! this module has to live on the side both can reach. The one
//! Cranelift-specific seam is `omega_codegen::cranelift::leaf::
//! cranelift_type`, which maps a [`Leaf`] onto `cranelift::Type` -- a
//! future backend adds its own equally small mapping, not another copy of
//! this file.
//!
//! Layout is packed by default -- each field sits at the raw running byte
//! sum of its predecessors -- unless `@layout(pack = ...)`/`@layout(align =
//! ...)` says otherwise somewhere in the type graph (see `type_alignment`/
//! `place_field`); x86_64 and aarch64 both tolerate unaligned loads/stores
//! with no correctness issue, so packed is safe as a default, it's just not
//! C-ABI-compatible layout -- this compiler doesn't implement true C-ABI
//! struct-passing conventions at function boundaries either (structs are
//! passed as flattened positional scalars, not per-platform aggregate
//! rules).

use crate::resolved_type::{ResolvedEnumType, ResolvedStructType, ResolvedType, ResolvedUnionType};

/// A single scalar machine value -- the backend-agnostic vocabulary every
/// backend's own native IR type maps onto. `Ptr`'s size depends on the
/// target, not the backend, so `Leaf::bytes` takes it explicitly rather
/// than assuming a width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Leaf {
    I8,
    I16,
    I32,
    I64,
    F32,
    F64,
    Ptr,
}

impl Leaf {
    pub fn bytes(self, pointer_bytes: u32) -> u32 {
        match self {
            Leaf::I8 => 1,
            Leaf::I16 => 2,
            Leaf::I32 => 4,
            Leaf::I64 => 8,
            Leaf::F32 => 4,
            Leaf::F64 => 8,
            Leaf::Ptr => pointer_bytes,
        }
    }
}

/// Flattens `ty` into its scalar leaves, in order -- the single source of
/// truth for how any value is represented, whether as a memory layout
/// (`layout_fields`'s byte offsets) or as a flat parameter/return-value
/// list (a backend's own calling convention).
pub fn leaves_of(ty: &ResolvedType, pointer_bytes: u32) -> Vec<Leaf> {
    match ty {
        ResolvedType::Void => vec![],
        // `Bool` is a plain 0/1 byte -- there's no dedicated boolean leaf
        // kind (see `ResolvedType::Bool`'s doc comment).
        ResolvedType::Bool => vec![Leaf::I8],
        // A decoded 4-byte Unicode scalar value, not a byte -- see
        // `ResolvedType::Char`'s doc comment for why this isn't `I8`.
        ResolvedType::Char => vec![Leaf::I32],
        ResolvedType::I8 | ResolvedType::U8 => vec![Leaf::I8],
        ResolvedType::I16 | ResolvedType::U16 => vec![Leaf::I16],
        ResolvedType::I32 | ResolvedType::U32 => vec![Leaf::I32],
        ResolvedType::I64 | ResolvedType::U64 => vec![Leaf::I64],
        // The only leaf whose real size is target-dependent rather than
        // fixed by the Omega type itself (see `ResolvedType::USize`/
        // `ISize`'s doc comments) -- `Leaf::Ptr` carries that
        // dependency, resolved by whoever asks for its `bytes()`.
        ResolvedType::USize | ResolvedType::ISize => vec![Leaf::Ptr],
        ResolvedType::F32 => vec![Leaf::F32],
        ResolvedType::F64 => vec![Leaf::F64],
        // Interior gaps (from a field's own transitive `align`, or from
        // this struct's own `@layout(pack = n)` chunking -- see
        // `place_field`) and any trailing padding this struct's own
        // `@layout(align = n)` demands are real filler `I8` leaves here,
        // not just a byte-offset bookkeeping detail: this leaf list is
        // also what a parameter struct value *is* (flattened positional
        // scalars), so the gaps have to actually exist as leaves for
        // `field_byte_offset`'s memory-side byte offsets and this
        // leaf-list's own positions to keep agreeing with each other.
        ResolvedType::Struct(struct_type) => {
            let struct_type = struct_type.borrow();
            let field_types: Vec<ResolvedType> = struct_type.fields.iter().map(|(_, t, _)| t.clone()).collect();
            let layout = layout_fields(&field_types, struct_type.layout.pack, pointer_bytes);
            let mut leaves = layout.leaves;
            let final_size = round_up(layout.packed_end, struct_type.layout.align);
            leaves.extend(std::iter::repeat_n(Leaf::I8, (final_size - layout.packed_end) as usize));
            leaves
        }
        // Every field overlaps the same storage -- exactly the shape a
        // single enum variant's payload has (see `enum_payload_bytes`'s
        // doc comment), so this reuses the same opaque-chunk flattening,
        // with no tag/header leaves in front of it. Unions don't support
        // `@layout` (see `ResolvedUnionType::suppress`'s doc comment), so
        // there's no alignment/padding concern here at all.
        ResolvedType::Union(union_type) => payload_chunks(union_bytes(&union_type.borrow(), pointer_bytes)),
        // An enum value is `[tag][header fields][shared dynamic fields]
        // [payload]` -- the tag, header, and shared dynamic fields all
        // flatten like ordinary struct fields (the dynamic fields are
        // simply ordinary per-instance storage, unlike the header's
        // per-variant constants), while the payload (a union of every
        // variant's body, sized to the largest) flattens to opaque
        // integer chunks: no single typed leaf list can describe a union,
        // so the chunks only ever move bytes around (assignment,
        // parameter passing); a body field is read/written through memory
        // at its byte offset instead. A statically-known variant
        // refinement never changes the layout -- every enum value is
        // full-size, which is exactly what makes refined -> plain
        // widening a plain leaf copy. Interior/trailing padding is
        // handled exactly like `Struct`'s arm above; the payload's own
        // start additionally respects the largest alignment any variant's
        // own body field demands (see `enum_payload_alignment`), since
        // every variant shares that one starting offset.
        ResolvedType::Enum { cell, .. } => {
            let enum_type = cell.borrow();
            let prefix = enum_prefix_layout(&enum_type, pointer_bytes);
            let mut leaves = prefix.leaves;

            let payload_align = enum_payload_alignment(&enum_type);
            let payload_size = enum_payload_bytes(&enum_type, enum_type.layout.pack, pointer_bytes);
            let payload_offset = place_field(prefix.packed_end, payload_align, payload_size, enum_type.layout.pack);
            leaves.extend(std::iter::repeat_n(Leaf::I8, (payload_offset - prefix.packed_end) as usize));
            leaves.extend(payload_chunks(payload_size));

            let final_size = round_up(payload_offset + payload_size, enum_type.layout.align);
            leaves.extend(std::iter::repeat_n(Leaf::I8, (final_size - (payload_offset + payload_size)) as usize));
            leaves
        }
        // `N` copies of the item type's own leaves, back to back -- the
        // same packed, no-padding layout a `Struct`'s fields get.
        ResolvedType::SizedArray(item_type, size) => {
            let item_leaves = leaves_of(item_type, pointer_bytes);
            std::iter::repeat_n(item_leaves, *size as usize).flatten().collect()
        }
        // A fat pointer: a data pointer plus an `i32` length. See
        // `ResolvedType::Slice`'s doc comment for why this is a distinct
        // variant rather than `Pointer(Array(_))`. `Str` shares the
        // identical leaf shape (see its own doc comment) but is kept a
        // separate arm rather than folded into this one, matching how
        // it's a fully separate `ResolvedType` variant, not a structural
        // alias.
        ResolvedType::Slice { .. } | ResolvedType::Str { .. } => vec![Leaf::Ptr, Leaf::I32],
        // `Pointer`, `Function`, and the legacy unsized `Array` (see its
        // doc comment) are all a single thin pointer value.
        ResolvedType::Pointer { .. } | ResolvedType::Function(_) | ResolvedType::Array(_) => vec![Leaf::Ptr],
        // `Spec` is a reference to a spec *definition*, never a runtime
        // value of its own -- it never actually reaches codegen (only
        // `SpecObject`, an actual value type, does); no leaves make sense
        // for it.
        ResolvedType::Spec(_) => unreachable!("a spec definition is never itself a value type"),
        // `spec *Animal`: a fat pointer, exactly like `Slice`'s
        // `[data_ptr, len]` shape above -- a data pointer plus a
        // compiler-generated vtable pointer, both plain thin pointers.
        ResolvedType::SpecObject { .. } => vec![Leaf::Ptr, Leaf::Ptr],
    }
}

/// Slices a `FieldAccess` projection's already-resolved `field_index` out
/// of an already-materialized value list (a parameter that hasn't been
/// dereferenced through -- positional, by leaf count, since there's no
/// memory/byte offset for a bare SSA value). No name search, no failure
/// path: the mir already picked this exact index out of `struct_type`.
pub fn project_field_access<T: Clone>(
    values: &[T],
    struct_type: &ResolvedStructType,
    field_index: usize,
    pointer_bytes: u32,
) -> Vec<T> {
    let field_types: Vec<ResolvedType> = struct_type.fields.iter().map(|(_, t, _)| t.clone()).collect();
    let start = layout_fields(&field_types, struct_type.layout.pack, pointer_bytes).leaf_starts[field_index];
    let len = leaves_of(&struct_type.fields[field_index].1, pointer_bytes).len();

    values[start..start + len].to_vec()
}

/// A resolved type's total in-memory size, in bytes: the sum of its scalar
/// leaves' sizes (`leaves_of` already flattens a struct/enum recursively
/// into its leaves -- interior/trailing padding included).
pub fn total_bytes(ty: &ResolvedType, pointer_bytes: u32) -> u32 {
    leaves_of(ty, pointer_bytes).iter().map(|leaf| leaf.bytes(pointer_bytes)).sum()
}

/// Whether `ty` occupies zero bytes -- used to reject a zero-field
/// `struct`/`union` (see `AnalysisErrorKind::ZeroSizedAggregate`), for
/// which a `marker` declaration exists instead. Deliberately independent of
/// any real target's pointer width: a leaf's own *existence* in
/// `leaves_of`'s result never depends on `pointer_bytes` (only a
/// `Leaf::Ptr` leaf's *byte size* does, via `total_bytes` above), so `0`
/// here is a safe placeholder rather than a real target width -- this is
/// what lets the analyzer call this without carrying pointer-width state
/// of its own (it doesn't have any; `sizeof<T>` is deferred to codegen for
/// exactly this reason).
pub fn is_zero_sized(ty: &ResolvedType) -> bool {
    leaves_of(ty, 0).is_empty()
}

/// A struct/enum's own alignment requirement when embedded as a field --
/// `1` (no alignment; the implicit default) for everything except an
/// explicit `@layout(align = n)` struct/enum, which imposes `n`. The
/// *only* source of alignment anywhere in this layout model: never
/// inferred from a primitive's own natural width, so a struct/enum with no
/// `@layout(align = ...)` anywhere in its own or its fields' types keeps a
/// fully packed layout. Unrelated to `pack` -- see `Layout`'s own doc
/// comment for why the two are orthogonal.
pub fn type_alignment(ty: &ResolvedType) -> u32 {
    match ty {
        ResolvedType::Struct(cell) => cell.borrow().layout.align,
        ResolvedType::Enum { cell, .. } => cell.borrow().layout.align,
        _ => 1,
    }
}

pub fn round_up(offset: u32, align: u32) -> u32 {
    if align <= 1 { offset } else { offset.div_ceil(align) * align }
}

/// Places one field at `offset`, honoring both its own transitive
/// alignment (`field_align`, from `type_alignment`) and the enclosing
/// type's own `pack` (see `omega_analyzer::annotations::Layout::pack`'s
/// doc comment): a chunk of size `pack` starts at every multiple of
/// `pack`; a field is placed at its own (already alignment-rounded) offset
/// if it fits in what remains of the chunk it would start in, *or* if it
/// would be the first thing placed in that chunk (`offset_in_chunk == 0`
/// -- without this, a single field bigger than `pack` itself could never
/// "fit" and would uselessly bounce to the next chunk boundary forever);
/// otherwise padding advances to the start of the next chunk. `pack == 1`
/// (the default) is a true no-op: every offset is already a multiple of
/// `1`, so `offset_in_chunk` is always `0` and every field lands at its
/// plain aligned offset -- byte-identical to this type's layout before
/// `@layout` existed at all.
pub fn place_field(offset: u32, field_align: u32, field_size: u32, pack: u32) -> u32 {
    let aligned = round_up(offset, field_align);
    let chunk_start = (aligned / pack) * pack;
    let offset_in_chunk = aligned - chunk_start;
    if offset_in_chunk == 0 || offset_in_chunk + field_size <= pack {
        aligned
    } else {
        round_up(chunk_start + pack, field_align)
    }
}

/// One field-sequence's full layout -- struct fields, an enum's own
/// `[tag, header..., dynamic...]` run, or a single variant's body fields.
/// Tracks *both* byte offsets (for memory-backed access -- `field_byte_
/// offset`, the enum offset functions below) and leaf-list start indices
/// (for register/SSA-value-backed access -- `project_field_access`, the
/// `EnumHeader`/`EnumDynamicField` projection arms): once an `@layout(
/// align = n)`/`@layout(pack = n)` field can insert a gap, the two stop
/// being derivable from each other by a flat per-field leaf-count sum, so
/// both are computed together, once, here -- the single source of truth
/// every other layout function reads from, so none of them can drift out
/// of agreement with each other or with `leaves_of`'s own `Struct`/`Enum`
/// arms (which use this directly).
pub struct FieldLayout {
    pub byte_offsets: Vec<u32>,
    pub leaf_starts: Vec<usize>,
    pub leaves: Vec<Leaf>,
    /// The packed-with-interior-layout running offset just past the last
    /// field -- *not* yet rounded up to the whole sequence's own trailing
    /// alignment (callers embedding a struct/enum's own `@layout(align =
    /// n)` round this up themselves; an enum's tag/header/dynamic run has
    /// no trailing alignment of its own at all, only the payload's start
    /// and the enum's overall end do).
    pub packed_end: u32,
}

/// `pack` is the *enclosing* struct/enum's own resolved `@layout(pack =
/// ...)` (see `place_field`) -- applied uniformly to every field in
/// `types`, whether this call is laying out a struct's own fields, an
/// enum's `[tag, header..., dynamic...]` run, or one variant's body
/// fields.
pub fn layout_fields(types: &[ResolvedType], pack: u32, pointer_bytes: u32) -> FieldLayout {
    let mut byte_offsets = Vec::with_capacity(types.len());
    let mut leaf_starts = Vec::with_capacity(types.len());
    let mut leaves = Vec::new();
    let mut offset = 0u32;
    for ty in types {
        let field_leaves = leaves_of(ty, pointer_bytes);
        let field_size = field_leaves.iter().map(|leaf| leaf.bytes(pointer_bytes)).sum::<u32>();
        let placed = place_field(offset, type_alignment(ty), field_size, pack);
        leaves.extend(std::iter::repeat_n(Leaf::I8, (placed - offset) as usize));
        byte_offsets.push(placed);
        leaf_starts.push(leaves.len());
        offset = placed + field_size;
        leaves.extend(field_leaves);
    }
    FieldLayout { byte_offsets, leaf_starts, leaves, packed_end: offset }
}

/// A `FieldAccess` projection's already-resolved `field_index`'s byte
/// offset within `struct_type` (honoring interior alignment/pack gaps --
/// see `place_field`) -- the memory-backed counterpart to
/// `project_field_access`'s positional (register/SSA-value) slicing.
pub fn field_byte_offset(struct_type: &ResolvedStructType, field_index: usize, pointer_bytes: u32) -> u32 {
    let field_types: Vec<ResolvedType> = struct_type.fields.iter().map(|(_, t, _)| t.clone()).collect();
    layout_fields(&field_types, struct_type.layout.pack, pointer_bytes).byte_offsets[field_index]
}

/// The size of an enum's payload region: the largest variant body, each
/// laid out via `layout_fields` with the enum's own `pack` (so a variant
/// whose own fields need internal alignment/pack-chunking is sized
/// correctly) -- `0` for an enum with no variant bodies at all.
pub fn enum_payload_bytes(enum_type: &ResolvedEnumType, pack: u32, pointer_bytes: u32) -> u32 {
    enum_type
        .variants
        .iter()
        .map(|v| {
            let field_types: Vec<ResolvedType> = v.fields.iter().map(|(_, t, _)| t.clone()).collect();
            layout_fields(&field_types, pack, pointer_bytes).packed_end
        })
        .max()
        .unwrap_or(0)
}

/// The largest alignment any variant's own body field demands -- every
/// variant's body shares the same starting offset (the payload region is
/// one shared union of all of them), so the payload's own start (see
/// `enum_payload_offset`) has to satisfy whichever variant needs the
/// most. `1` (no alignment) when no variant has any field demanding one.
pub fn enum_payload_alignment(enum_type: &ResolvedEnumType) -> u32 {
    enum_type.variants.iter().flat_map(|v| v.fields.iter().map(|(_, t, _)| type_alignment(t))).max().unwrap_or(1)
}

/// The enum's own `[tag, header..., dynamic...]` run, laid out (with the
/// enum's own `@layout(pack = ...)`) as one `layout_fields` sequence --
/// shared by every offset function below, so `enum_header_offset`/
/// `enum_dynamic_field_offset`/`enum_payload_offset` (and their
/// register/SSA-value-backed counterparts in the projection-handling
/// code) all index into the exact same layout `leaves_of`'s `Enum` arm
/// built its leaves from.
pub fn enum_prefix_layout(enum_type: &ResolvedEnumType, pointer_bytes: u32) -> FieldLayout {
    let mut types = vec![enum_type.tag_type.clone()];
    types.extend(enum_type.header.iter().map(|(_, t, _)| t.clone()));
    types.extend(enum_type.dynamic_fields.iter().map(|(_, t, _)| t.clone()));
    layout_fields(&types, enum_type.layout.pack, pointer_bytes)
}

/// The size of a union's storage: its largest field, in packed bytes --
/// `0` for a union with no fields at all. See `enum_payload_bytes`, whose
/// shape this mirrors exactly (a union's whole body plays the same role a
/// single enum variant's body does).
pub fn union_bytes(union_type: &ResolvedUnionType, pointer_bytes: u32) -> u32 {
    union_type.fields.iter().map(|(_, ty, _)| total_bytes(ty, pointer_bytes)).max().unwrap_or(0)
}

/// Decomposes an enum's (or union's) payload region into opaque integer
/// leaves covering exactly `bytes` -- as many `I64`s as fit, then one
/// `I32`/`I16`/`I8` as needed. Deterministic and layout-only: these leaves
/// exist so the payload can ride the same flattened-scalar machinery
/// every other value uses (copies, params, returns); nothing ever
/// interprets them as numbers.
pub fn payload_chunks(mut bytes: u32) -> Vec<Leaf> {
    let mut chunks = Vec::new();
    while bytes >= 8 {
        chunks.push(Leaf::I64);
        bytes -= 8;
    }
    if bytes >= 4 {
        chunks.push(Leaf::I32);
        bytes -= 4;
    }
    if bytes >= 2 {
        chunks.push(Leaf::I16);
        bytes -= 2;
    }
    if bytes >= 1 {
        chunks.push(Leaf::I8);
    }
    chunks
}

/// A header field's byte offset within an enum value (honoring interior
/// alignment/pack gaps) -- past the tag and every preceding header field.
/// Index `1 + index` into `enum_prefix_layout`'s combined run: index `0`
/// is always the tag.
pub fn enum_header_offset(enum_type: &ResolvedEnumType, index: usize, pointer_bytes: u32) -> u32 {
    enum_prefix_layout(enum_type, pointer_bytes).byte_offsets[1 + index]
}

/// A shared dynamic field's byte offset within an enum value -- past the
/// tag, the whole header, and every preceding dynamic field. Mirrors
/// `enum_header_offset` exactly, one region further into the same
/// combined run.
pub fn enum_dynamic_field_offset(enum_type: &ResolvedEnumType, index: usize, pointer_bytes: u32) -> u32 {
    enum_prefix_layout(enum_type, pointer_bytes).byte_offsets[1 + enum_type.header.len() + index]
}

/// The payload region's byte offset within an enum value -- past the tag,
/// the whole header, and the whole shared-dynamic-fields region, placed
/// (via `place_field`, honoring both the enum's own `pack` and whatever
/// alignment the largest variant field demands -- see
/// `enum_payload_alignment`) right after the prefix run -- every
/// variant's body shares this one starting offset.
pub fn enum_payload_offset(enum_type: &ResolvedEnumType, pointer_bytes: u32) -> u32 {
    let prefix = enum_prefix_layout(enum_type, pointer_bytes);
    let payload_size = enum_payload_bytes(enum_type, enum_type.layout.pack, pointer_bytes);
    place_field(prefix.packed_end, enum_payload_alignment(enum_type), payload_size, enum_type.layout.pack)
}

/// A body field's byte offset within an enum value: the payload region's
/// start plus every preceding field of the *same variant*, honoring
/// interior alignment/pack gaps within that variant's own fields (each
/// variant's body independently starts at the payload's start -- that's
/// the union).
pub fn enum_body_field_offset(
    enum_type: &ResolvedEnumType,
    variant_index: usize,
    field_index: usize,
    pointer_bytes: u32,
) -> u32 {
    let field_types: Vec<ResolvedType> =
        enum_type.variants[variant_index].fields.iter().map(|(_, t, _)| t.clone()).collect();
    enum_payload_offset(enum_type, pointer_bytes)
        + layout_fields(&field_types, enum_type.layout.pack, pointer_bytes).byte_offsets[field_index]
}

/// The alignment *shift* a backend's own stack-slot API wants (`2^shift`
/// bytes) for a value whose own required alignment (`type_alignment`) is
/// `align` bytes (always a power of two, or `1` -- see
/// `annotations::resolve_layout`'s validation). Never lower than `4` (16
/// bytes) -- every stack slot's baseline -- so this is a pure no-op for
/// the overwhelming common case (no `@layout(align = ...)` anywhere,
/// where `align` is `1`); only a `@layout(align = n)` type with `n > 16`
/// raises it further. Not Cranelift-specific despite the "stack slot"
/// framing -- any backend that allocates target-aligned local storage
/// needs the same shift-from-byte-alignment conversion.
pub fn stack_align_shift(align: u32) -> u8 {
    align.max(1).ilog2().max(4) as u8
}
