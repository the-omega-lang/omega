use crate::resolved_type::{ResolvedEnumType, ResolvedStructType, ResolvedType, ResolvedUnionType};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Leaf {
    I8,
    I16,
    I32,
    I64,
    F32,
    F64,
    Ptr,
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

pub fn leaves_of(ty: &ResolvedType, pointer_bytes: u32) -> Vec<Leaf> {
    match ty {
        ResolvedType::Void | ResolvedType::Never => vec![],
        ResolvedType::Bool => vec![Leaf::I8],
        ResolvedType::Char => vec![Leaf::I32],
        ResolvedType::I8 | ResolvedType::U8 => vec![Leaf::I8],
        ResolvedType::I16 | ResolvedType::U16 => vec![Leaf::I16],
        ResolvedType::I32 | ResolvedType::U32 => vec![Leaf::I32],
        ResolvedType::I64 | ResolvedType::U64 => vec![Leaf::I64],
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
            let field_types: Vec<ResolvedType> = struct_type
                .fields
                .iter()
                .map(|field| field.r#type.clone())
                .collect();
            let layout = layout_fields(&field_types, struct_type.layout.pack, pointer_bytes);
            let mut leaves = layout.leaves;
            let final_size = round_up(layout.packed_end, struct_type.layout.align);
            leaves.extend(std::iter::repeat_n(
                Leaf::I8,
                (final_size - layout.packed_end) as usize,
            ));
            leaves
        }
        ResolvedType::Union(union_type) => {
            payload_chunks(union_bytes(&union_type.borrow(), pointer_bytes))
        }
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
            let payload_offset = place_field(
                prefix.packed_end,
                payload_align,
                payload_size,
                enum_type.layout.pack,
            );
            leaves.extend(std::iter::repeat_n(
                Leaf::I8,
                (payload_offset - prefix.packed_end) as usize,
            ));
            leaves.extend(payload_chunks(payload_size));

            let final_size = round_up(payload_offset + payload_size, enum_type.layout.align);
            leaves.extend(std::iter::repeat_n(
                Leaf::I8,
                (final_size - (payload_offset + payload_size)) as usize,
            ));
            leaves
        }
        ResolvedType::SizedArray(item_type, size) => {
            let item_leaves = leaves_of(item_type, pointer_bytes);
            std::iter::repeat_n(item_leaves, *size as usize)
                .flatten()
                .collect()
        }
        ResolvedType::Slice { .. } | ResolvedType::Str { .. } => vec![Leaf::Ptr, Leaf::I32],
        ResolvedType::Pointer { .. } | ResolvedType::Function(_) | ResolvedType::Array(_, _) => {
            vec![Leaf::Ptr]
        }
        ResolvedType::Spec(_) => unreachable!("a spec definition is never itself a value type"),
        ResolvedType::SpecObject { .. } => vec![Leaf::Ptr, Leaf::Ptr],
    }
}

pub fn project_field_access<T: Clone>(
    values: &[T],
    struct_type: &ResolvedStructType,
    field_index: usize,
    pointer_bytes: u32,
) -> Vec<T> {
    let field_types: Vec<ResolvedType> = struct_type
        .fields
        .iter()
        .map(|field| field.r#type.clone())
        .collect();
    let start = layout_fields(&field_types, struct_type.layout.pack, pointer_bytes).leaf_starts
        [field_index];
    let len = leaves_of(&struct_type.fields[field_index].r#type, pointer_bytes).len();

    values[start..start + len].to_vec()
}

pub fn total_bytes(ty: &ResolvedType, pointer_bytes: u32) -> u32 {
    leaves_of(ty, pointer_bytes)
        .iter()
        .map(|leaf| leaf.bytes(pointer_bytes))
        .sum()
}

pub fn is_zero_sized(ty: &ResolvedType) -> bool {
    leaves_of(ty, 0).is_empty()
}

pub fn type_alignment(ty: &ResolvedType) -> u32 {
    match ty {
        ResolvedType::Struct(cell) => cell.borrow().layout.align,
        ResolvedType::Enum { cell, .. } => cell.borrow().layout.align,
        _ => 1,
    }
}

pub fn round_up(offset: u32, align: u32) -> u32 {
    if align <= 1 {
        offset
    } else {
        offset.div_ceil(align) * align
    }
}

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

pub struct FieldLayout {
    pub byte_offsets: Vec<u32>,
    pub leaf_starts: Vec<usize>,
    pub leaves: Vec<Leaf>,
    pub packed_end: u32,
}

pub fn layout_fields(types: &[ResolvedType], pack: u32, pointer_bytes: u32) -> FieldLayout {
    let mut byte_offsets = Vec::with_capacity(types.len());
    let mut leaf_starts = Vec::with_capacity(types.len());
    let mut leaves = Vec::new();
    let mut offset = 0u32;
    for ty in types {
        let field_leaves = leaves_of(ty, pointer_bytes);
        let field_size = field_leaves
            .iter()
            .map(|leaf| leaf.bytes(pointer_bytes))
            .sum::<u32>();
        let placed = place_field(offset, type_alignment(ty), field_size, pack);
        leaves.extend(std::iter::repeat_n(Leaf::I8, (placed - offset) as usize));
        byte_offsets.push(placed);
        leaf_starts.push(leaves.len());
        offset = placed + field_size;
        leaves.extend(field_leaves);
    }
    FieldLayout {
        byte_offsets,
        leaf_starts,
        leaves,
        packed_end: offset,
    }
}

pub fn field_byte_offset(
    struct_type: &ResolvedStructType,
    field_index: usize,
    pointer_bytes: u32,
) -> u32 {
    let field_types: Vec<ResolvedType> = struct_type
        .fields
        .iter()
        .map(|field| field.r#type.clone())
        .collect();
    layout_fields(&field_types, struct_type.layout.pack, pointer_bytes).byte_offsets[field_index]
}

pub fn locals_layout(local_types: &[ResolvedType], pointer_bytes: u32) -> FieldLayout {
    layout_fields(local_types, 1, pointer_bytes)
}

pub fn enum_payload_bytes(enum_type: &ResolvedEnumType, pack: u32, pointer_bytes: u32) -> u32 {
    enum_type
        .variants
        .iter()
        .map(|v| {
            let field_types: Vec<ResolvedType> =
                v.fields.iter().map(|field| field.r#type.clone()).collect();
            layout_fields(&field_types, pack, pointer_bytes).packed_end
        })
        .max()
        .unwrap_or(0)
}

pub fn enum_payload_alignment(enum_type: &ResolvedEnumType) -> u32 {
    enum_type
        .variants
        .iter()
        .flat_map(|v| v.fields.iter().map(|field| type_alignment(&field.r#type)))
        .max()
        .unwrap_or(1)
}

pub fn enum_prefix_layout(enum_type: &ResolvedEnumType, pointer_bytes: u32) -> FieldLayout {
    let mut types = vec![enum_type.tag_type.clone()];
    types.extend(enum_type.header.iter().map(|field| field.r#type.clone()));
    types.extend(
        enum_type
            .dynamic_fields
            .iter()
            .map(|field| field.r#type.clone()),
    );
    layout_fields(&types, enum_type.layout.pack, pointer_bytes)
}

pub fn union_bytes(union_type: &ResolvedUnionType, pointer_bytes: u32) -> u32 {
    union_type
        .fields
        .iter()
        .map(|field| total_bytes(&field.r#type, pointer_bytes))
        .max()
        .unwrap_or(0)
}

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

pub fn enum_header_offset(enum_type: &ResolvedEnumType, index: usize, pointer_bytes: u32) -> u32 {
    enum_prefix_layout(enum_type, pointer_bytes).byte_offsets[1 + index]
}

pub fn enum_dynamic_field_offset(
    enum_type: &ResolvedEnumType,
    index: usize,
    pointer_bytes: u32,
) -> u32 {
    enum_prefix_layout(enum_type, pointer_bytes).byte_offsets[1 + enum_type.header.len() + index]
}

pub fn enum_payload_offset(enum_type: &ResolvedEnumType, pointer_bytes: u32) -> u32 {
    let prefix = enum_prefix_layout(enum_type, pointer_bytes);
    let payload_size = enum_payload_bytes(enum_type, enum_type.layout.pack, pointer_bytes);
    place_field(
        prefix.packed_end,
        enum_payload_alignment(enum_type),
        payload_size,
        enum_type.layout.pack,
    )
}

pub fn enum_body_field_offset(
    enum_type: &ResolvedEnumType,
    variant_index: usize,
    field_index: usize,
    pointer_bytes: u32,
) -> u32 {
    let field_types: Vec<ResolvedType> = enum_type.variants[variant_index]
        .fields
        .iter()
        .map(|field| field.r#type.clone())
        .collect();
    enum_payload_offset(enum_type, pointer_bytes)
        + layout_fields(&field_types, enum_type.layout.pack, pointer_bytes).byte_offsets
            [field_index]
}

pub fn stack_align_shift(align: u32) -> u8 {
    align.max(1).ilog2().max(4) as u8
}
