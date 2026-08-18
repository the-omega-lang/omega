//! Backend-agnostic struct/enum/union layout math: byte offsets, padding,
//! and leaf-flattening for any [`ResolvedType`]. Every function here is
//! pure data computation over `ResolvedType` and a target's pointer width
//! -- nothing names a specific backend's native IR type, so a second
//! backend calls straight into this instead of re-deriving struct/enum
//! layout from scratch. Lives in `omega-analyzer` rather than
//! `omega-codegen` (where it originated) so a `comp` evaluation's own
//! `sizeof` support can call it too, since `omega-codegen` depends on
//! `omega-analyzer`, never the reverse. The one backend-specific seam is
//! `omega_codegen::cranelift::leaf::cranelift_type`, mapping a [`Leaf`]
//! onto `cranelift::Type`; a future backend adds its own equally small
//! mapping, not another copy of this file.
//!
//! Layout is packed by default -- each field sits at the raw running byte
//! sum of its predecessors -- unless `@layout(pack = ...)`/`@layout(align =
//! ...)` says otherwise; x86_64 and aarch64 both tolerate unaligned
//! loads/stores, so packed is a safe default, just not C-ABI-compatible
//! (this compiler doesn't implement true C-ABI struct-passing conventions
//! at function boundaries either -- structs pass as flattened positional
//! scalars).

use crate::resolved_type::{ResolvedEnumType, ResolvedStructType, ResolvedType, ResolvedUnionType};

/// A single scalar machine value -- the backend-agnostic vocabulary every
/// backend's own native IR type maps onto. `Ptr`/`Size`'s width depends on
/// the target, not the backend, so `Leaf::bytes` takes it explicitly.
///
/// `Ptr` and `Size` are the *same width* and differ only in domain: `Ptr`
/// is a genuine address, `Size` is a pointer-width *integer*
/// (`usize`/`isize`). Cranelift can conflate them (its `pointer_type()` is
/// simply an integer type, both map to `I64`); LLVM cannot -- `ptr` and
/// `iN` are distinct types, and typing a `usize` as `ptr` makes every
/// size-typed integer an opaque pointer, breaking arithmetic on it and
/// misinforming alias analysis. The distinction lives here rather than
/// being re-guessed per backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Leaf {
    I8,
    I16,
    I32,
    I64,
    F32,
    F64,
    Ptr,
    /// A pointer-width integer -- see the type-level comment.
    Size,
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
            Leaf::Ptr | Leaf::Size => pointer_bytes,
        }
    }
}

/// Flattens `ty` into its scalar leaves, in order -- the single source of
/// truth for how any value is represented, whether as a memory layout or
/// as a flat parameter/return-value list.
pub fn leaves_of(ty: &ResolvedType, pointer_bytes: u32) -> Vec<Leaf> {
    match ty {
        // Same as `Void`: nothing ever materializes a `Never` value, so
        // there's nothing to flatten.
        ResolvedType::Void | ResolvedType::Never => vec![],
        // Plain 0/1 byte -- no dedicated boolean leaf kind.
        ResolvedType::Bool => vec![Leaf::I8],
        // A decoded 4-byte Unicode scalar value, not a byte.
        ResolvedType::Char => vec![Leaf::I32],
        ResolvedType::I8 | ResolvedType::U8 => vec![Leaf::I8],
        ResolvedType::I16 | ResolvedType::U16 => vec![Leaf::I16],
        ResolvedType::I32 | ResolvedType::U32 => vec![Leaf::I32],
        ResolvedType::I64 | ResolvedType::U64 => vec![Leaf::I64],
        // Target-dependent in width, which `Leaf::Size` carries. `Size`,
        // not `Ptr`: these are pointer-width integers, not addresses.
        ResolvedType::USize | ResolvedType::ISize => vec![Leaf::Size],
        ResolvedType::F32 => vec![Leaf::F32],
        ResolvedType::F64 => vec![Leaf::F64],
        // Interior gaps and trailing padding are real filler `I8` leaves
        // here, not just byte-offset bookkeeping: this leaf list is also
        // what a parameter struct value *is* (flattened positional
        // scalars), so `field_byte_offset`'s offsets and this list's
        // positions must agree.
        ResolvedType::Struct(struct_type) => {
            let struct_type = struct_type.borrow();
            let field_types: Vec<ResolvedType> = struct_type.fields.iter().map(|(_, t, _)| t.clone()).collect();
            let layout = layout_fields(&field_types, struct_type.layout.pack, pointer_bytes);
            let mut leaves = layout.leaves;
            let final_size = round_up(layout.packed_end, struct_type.layout.align);
            leaves.extend(std::iter::repeat_n(Leaf::I8, (final_size - layout.packed_end) as usize));
            leaves
        }
        // Every field overlaps the same storage, the shape a single enum
        // variant's payload has, so this reuses the same opaque-chunk
        // flattening. Unions don't support `@layout`, so no
        // alignment/padding concern here.
        ResolvedType::Union(union_type) => payload_chunks(union_bytes(&union_type.borrow(), pointer_bytes)),
        // An enum value is `[tag][header fields][shared dynamic fields]
        // [payload]` -- tag/header/dynamic fields flatten like ordinary
        // struct fields, while the payload (a union of every variant's
        // body, sized to the largest) flattens to opaque integer chunks:
        // no single typed leaf list can describe a union, so a body field
        // is read/written through memory at its byte offset instead. A
        // statically-known variant refinement never changes the layout --
        // every enum value is full-size, which is what makes refined ->
        // plain widening a plain leaf copy. The payload's start also
        // respects the largest alignment any variant's body field demands
        // (see `enum_payload_alignment`), since every variant shares that
        // one starting offset.
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
        // `N` copies of the item type's own leaves, back to back.
        ResolvedType::SizedArray(item_type, size) => {
            let item_leaves = leaves_of(item_type, pointer_bytes);
            std::iter::repeat_n(item_leaves, *size as usize).flatten().collect()
        }
        // A fat pointer: a data pointer plus an `i32` length. `Str` shares
        // the identical leaf shape but is kept a separate arm, matching
        // how it's a fully separate `ResolvedType` variant.
        ResolvedType::Slice { .. } | ResolvedType::Str { .. } => vec![Leaf::Ptr, Leaf::I32],
        // `Pointer`, `Function`, and the fully general `Array` are all a
        // single thin pointer value.
        ResolvedType::Pointer { .. } | ResolvedType::Function(_) | ResolvedType::Array(_, _) => vec![Leaf::Ptr],
        // A reference to a spec *definition*, never a runtime value of its
        // own -- it never reaches codegen (only `SpecObject` does).
        ResolvedType::Spec(_) => unreachable!("a spec definition is never itself a value type"),
        // `spec *Animal`: a fat pointer, a data pointer plus a
        // compiler-generated vtable pointer, both thin pointers.
        ResolvedType::SpecObject { .. } => vec![Leaf::Ptr, Leaf::Ptr],
    }
}

/// Slices a `FieldAccess` projection's already-resolved `field_index` out
/// of an already-materialized value list (a parameter that hasn't been
/// dereferenced through -- positional, by leaf count, since there's no
/// memory/byte offset for a bare SSA value).
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

/// A resolved type's total in-memory size, in bytes: the sum of its
/// scalar leaves' sizes (`leaves_of` already flattens a struct/enum
/// recursively, interior/trailing padding included).
pub fn total_bytes(ty: &ResolvedType, pointer_bytes: u32) -> u32 {
    leaves_of(ty, pointer_bytes).iter().map(|leaf| leaf.bytes(pointer_bytes)).sum()
}

/// Whether `ty` occupies zero bytes -- used to reject a zero-field
/// `struct`/`union` (`marker` exists for that case instead). Independent
/// of any real target's pointer width: a leaf's *existence* never depends
/// on `pointer_bytes` (only its byte size does), so `0` is a safe
/// placeholder -- this lets the analyzer call this without carrying
/// pointer-width state of its own.
pub fn is_zero_sized(ty: &ResolvedType) -> bool {
    leaves_of(ty, 0).is_empty()
}

/// A struct/enum's own alignment requirement when embedded as a field --
/// `1` (no alignment) unless an explicit `@layout(align = n)` imposes `n`.
/// The only source of alignment anywhere in this layout model: never
/// inferred from a primitive's natural width. Unrelated to `pack` -- see
/// `Layout`'s doc comment for why the two are orthogonal.
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
/// alignment (`field_align`) and the enclosing type's own `pack`: a chunk
/// of size `pack` starts at every multiple of `pack`; a field is placed at
/// its own aligned offset if it fits in what remains of that chunk, or if
/// it would be the first thing placed in it (`offset_in_chunk == 0` --
/// without this, a field bigger than `pack` could never "fit" and would
/// bounce to the next chunk boundary forever); otherwise padding advances
/// to the start of the next chunk. `pack == 1` (the default) is a true
/// no-op: every field lands at its plain aligned offset, byte-identical to
/// this type's layout before `@layout` existed.
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
/// Tracks *both* byte offsets (memory-backed access) and leaf-list start
/// indices (register/SSA-value-backed access): once an `@layout` field can
/// insert a gap, the two stop being derivable from each other, so both are
/// computed together, once, here -- the single source of truth every
/// other layout function reads from.
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

/// `pack` is the enclosing struct/enum's own resolved `@layout(pack =
/// ...)`, applied uniformly to every field in `types`, whether laying out
/// a struct's fields, an enum's `[tag, header..., dynamic...]` run, or one
/// variant's body fields.
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
/// offset within `struct_type` -- the memory-backed counterpart to
/// `project_field_access`'s positional (register/SSA-value) slicing.
pub fn field_byte_offset(struct_type: &ResolvedStructType, field_index: usize, pointer_bytes: u32) -> u32 {
    let field_types: Vec<ResolvedType> = struct_type.fields.iter().map(|(_, t, _)| t.clone()).collect();
    layout_fields(&field_types, struct_type.layout.pack, pointer_bytes).byte_offsets[field_index]
}

/// A function's own non-parameter locals (`MirBody::locals[arg_count..]`),
/// laid out exactly like a struct's fields -- `pack = 1`, the default an
/// unannotated struct uses. Deliberately the *only* place a function's
/// stack frame is laid out: every backend calls this one function instead
/// of independently deciding its own per-local placement, so "a
/// zero-sized local's address is wherever it would be if it existed" is a
/// property of this shared algorithm, not an accident of whichever
/// backend's stack-slot allocator is compiling a given function.
pub fn locals_layout(local_types: &[ResolvedType], pointer_bytes: u32) -> FieldLayout {
    layout_fields(local_types, 1, pointer_bytes)
}

/// The size of an enum's payload region: the largest variant body, each
/// laid out via `layout_fields` with the enum's own `pack` -- `0` for an
/// enum with no variant bodies.
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
/// one shared union of all of them), so the payload's start has to
/// satisfy whichever variant needs the most.
pub fn enum_payload_alignment(enum_type: &ResolvedEnumType) -> u32 {
    enum_type.variants.iter().flat_map(|v| v.fields.iter().map(|(_, t, _)| type_alignment(t))).max().unwrap_or(1)
}

/// The enum's own `[tag, header..., dynamic...]` run, laid out as one
/// `layout_fields` sequence -- shared by every offset function below, so
/// they all index into the exact same layout `leaves_of`'s `Enum` arm
/// built its leaves from.
pub fn enum_prefix_layout(enum_type: &ResolvedEnumType, pointer_bytes: u32) -> FieldLayout {
    let mut types = vec![enum_type.tag_type.clone()];
    types.extend(enum_type.header.iter().map(|(_, t, _)| t.clone()));
    types.extend(enum_type.dynamic_fields.iter().map(|(_, t, _)| t.clone()));
    layout_fields(&types, enum_type.layout.pack, pointer_bytes)
}

/// The size of a union's storage: its largest field, in packed bytes --
/// `0` for a union with no fields. Mirrors `enum_payload_bytes` (a
/// union's whole body plays the role a single enum variant's body does).
pub fn union_bytes(union_type: &ResolvedUnionType, pointer_bytes: u32) -> u32 {
    union_type.fields.iter().map(|(_, ty, _)| total_bytes(ty, pointer_bytes)).max().unwrap_or(0)
}

/// Decomposes an enum's (or union's) payload region into opaque integer
/// leaves covering exactly `bytes` -- as many `I64`s as fit, then one
/// `I32`/`I16`/`I8` as needed. These leaves exist so the payload can ride
/// the same flattened-scalar machinery every other value uses; nothing
/// ever interprets them as numbers.
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

/// A header field's byte offset within an enum value -- past the tag and
/// every preceding header field. Index `1 + index` into
/// `enum_prefix_layout`'s combined run: index `0` is always the tag.
pub fn enum_header_offset(enum_type: &ResolvedEnumType, index: usize, pointer_bytes: u32) -> u32 {
    enum_prefix_layout(enum_type, pointer_bytes).byte_offsets[1 + index]
}

/// A shared dynamic field's byte offset within an enum value -- past the
/// tag, the whole header, and every preceding dynamic field. Mirrors
/// `enum_header_offset`, one region further into the same run.
pub fn enum_dynamic_field_offset(enum_type: &ResolvedEnumType, index: usize, pointer_bytes: u32) -> u32 {
    enum_prefix_layout(enum_type, pointer_bytes).byte_offsets[1 + enum_type.header.len() + index]
}

/// The payload region's byte offset within an enum value -- past the tag,
/// header, and shared-dynamic-fields region, placed (honoring both the
/// enum's own `pack` and the largest variant-field alignment) right after
/// the prefix run -- every variant's body shares this one starting
/// offset.
pub fn enum_payload_offset(enum_type: &ResolvedEnumType, pointer_bytes: u32) -> u32 {
    let prefix = enum_prefix_layout(enum_type, pointer_bytes);
    let payload_size = enum_payload_bytes(enum_type, enum_type.layout.pack, pointer_bytes);
    place_field(prefix.packed_end, enum_payload_alignment(enum_type), payload_size, enum_type.layout.pack)
}

/// A body field's byte offset within an enum value: the payload
/// region's start plus every preceding field of the *same variant* (each
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
/// bytes) for a value whose required alignment (`type_alignment`) is
/// `align` bytes. Never lower than `4` (16 bytes, every stack slot's
/// baseline) -- a no-op for the common case (`align == 1`); only a
/// `@layout(align = n)` type with `n > 16` raises it further.
pub fn stack_align_shift(align: u32) -> u8 {
    align.max(1).ilog2().max(4) as u8
}
