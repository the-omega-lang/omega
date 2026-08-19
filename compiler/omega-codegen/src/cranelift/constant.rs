use super::Codegen;
use super::leaf::IntoCraneliftLeaves;
use super::place::PlaceStorage;
use cranelift::prelude::{
    FunctionBuilder, InstBuilder, StackSlotData, StackSlotKind, Value, types,
};
use cranelift_module::{DataDescription, DataId, Module};
use omega_analyzer::checked::NumberValue;
use omega_analyzer::layout;
use omega_analyzer::resolved_type::{ConstValue, ResolvedType};

fn ref_pointee_type(inner: &ConstValue, leaf_type: &ResolvedType) -> ResolvedType {
    match inner {
        ConstValue::Array(elements) => {
            ResolvedType::SizedArray(Box::new(leaf_type.clone()), elements.len() as u32)
        }
        _ => leaf_type.clone(),
    }
}

impl Codegen {
    pub(super) fn emit_bytes(&mut self, builder: &mut FunctionBuilder, s: String) -> Vec<Value> {
        let len = builder.ins().iconst(types::I32, s.len() as i64);

        let ptr_type = self.pointer_type();
        let data_id = if let Some(id) = self.bytes.get(&s) {
            *id
        } else {
            self.get_or_declare_global_bytes(s.clone())
        };

        let global_value = self.module.declare_data_in_func(data_id, builder.func);
        let ptr = builder.ins().global_value(ptr_type, global_value);

        vec![ptr, len]
    }

    fn data_symbol(bytes: &[u8]) -> String {
        omega_mir::mangle::data_symbol(bytes)
    }

    fn get_or_declare_global_bytes(&mut self, s: String) -> DataId {
        let bytes = s.clone().into_bytes();
        let sym = Self::data_symbol(&bytes);
        let id = self
            .module
            .declare_data(&sym, cranelift_module::Linkage::Preemptible, false, false)
            .unwrap();

        let mut desc = DataDescription::new();
        desc.define(bytes.into_boxed_slice());
        self.module.define_data(id, &desc).unwrap();

        self.bytes.insert(s, id);

        id
    }

    pub(super) fn emit_const_value(
        &mut self,
        builder: &mut FunctionBuilder,
        value: &ConstValue,
        r#type: &ResolvedType,
    ) -> Vec<Value> {
        match value {
            ConstValue::Number(number) => {
                let leaf = r#type.cranelift_leaves(self)[0];
                vec![match number {
                    NumberValue::Signed(v) => builder.ins().iconst(leaf, *v),
                    NumberValue::Unsigned(v) => builder.ins().iconst(leaf, *v as i64),
                    NumberValue::Float(v) if leaf == types::F32 => {
                        builder.ins().f32const(*v as f32)
                    }
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
            // Materialize compile-time aggregate fields in declared layout order.
            ConstValue::Struct(fields) => {
                let ResolvedType::Struct(struct_type) = r#type else {
                    unreachable!("a Struct constant's own type is always ResolvedType::Struct");
                };
                let field_types: Vec<ResolvedType> = struct_type
                    .borrow()
                    .fields
                    .iter()
                    .map(|field| field.r#type.clone())
                    .collect();
                fields
                    .iter()
                    .zip(&field_types)
                    .flat_map(|(value, field_type)| {
                        self.emit_const_value(builder, value, field_type)
                    })
                    .collect()
            }
            // Materialize enum tag/prefix/payload according to shared enum layout.
            ConstValue::Enum {
                variant_index,
                tag,
                dynamic_fields,
                fields,
                ..
            } => {
                let ResolvedType::Enum { cell, .. } = r#type else {
                    unreachable!("an Enum constant's own type is always ResolvedType::Enum");
                };
                let pointer_bytes = self.pointer_bytes();
                let (tag_type, header, dynamic, body) = {
                    let enum_type = cell.borrow();
                    let variant = &enum_type.variants[*variant_index];
                    let header: Vec<(ResolvedType, ConstValue)> = enum_type
                        .header
                        .iter()
                        .zip(&variant.header_values)
                        .map(|(field, value)| (field.r#type.clone(), value.clone()))
                        .collect();
                    let dynamic: Vec<(u32, ResolvedType)> = (0..enum_type.dynamic_fields.len())
                        .map(|i| {
                            let offset =
                                layout::enum_dynamic_field_offset(&enum_type, i, pointer_bytes);
                            (offset, enum_type.dynamic_fields[i].r#type.clone())
                        })
                        .collect();
                    let body: Vec<(u32, ResolvedType)> = (0..variant.fields.len())
                        .map(|i| {
                            let offset = layout::enum_body_field_offset(
                                &enum_type,
                                *variant_index,
                                i,
                                pointer_bytes,
                            );
                            (offset, variant.fields[i].r#type.clone())
                        })
                        .collect();
                    (enum_type.tag_type.clone(), header, dynamic, body)
                };

                let shift = layout::stack_align_shift(layout::type_alignment(r#type));
                let total = layout::total_bytes(r#type, pointer_bytes);
                let slot = builder.create_sized_stack_slot(StackSlotData::new(
                    StackSlotKind::ExplicitSlot,
                    total,
                    shift,
                ));

                let tag_values =
                    self.emit_const_value(builder, &ConstValue::Number(*tag), &tag_type);
                self.store_scalars(
                    builder,
                    &PlaceStorage::Slot { slot, offset: 0 },
                    &tag_values,
                );

                let mut offset = layout::total_bytes(&tag_type, pointer_bytes);
                for (field_type, value) in &header {
                    let values = self.emit_const_value(builder, value, field_type);
                    self.store_scalars(builder, &PlaceStorage::Slot { slot, offset }, &values);
                    offset += layout::total_bytes(field_type, pointer_bytes);
                }

                for (value, (field_offset, field_type)) in dynamic_fields.iter().zip(&dynamic) {
                    let values = self.emit_const_value(builder, value, field_type);
                    self.store_scalars(
                        builder,
                        &PlaceStorage::Slot {
                            slot,
                            offset: *field_offset,
                        },
                        &values,
                    );
                }

                for (value, (field_offset, field_type)) in fields.iter().zip(&body) {
                    let values = self.emit_const_value(builder, value, field_type);
                    self.store_scalars(
                        builder,
                        &PlaceStorage::Slot {
                            slot,
                            offset: *field_offset,
                        },
                        &values,
                    );
                }

                self.load_scalars(builder, &PlaceStorage::Slot { slot, offset: 0 }, r#type)
            }
            // Materialize the active union field at offset zero in shared union storage.
            ConstValue::Union { value, .. } => {
                let total = layout::total_bytes(r#type, self.pointer_bytes());
                let slot = builder.create_sized_stack_slot(StackSlotData::new(
                    StackSlotKind::ExplicitSlot,
                    total,
                    4,
                ));

                let mut chunk_offset = 0u32;
                for chunk in r#type.cranelift_leaves(self) {
                    let zero = builder.ins().iconst(chunk, 0);
                    builder.ins().stack_store(zero, slot, chunk_offset as i32);
                    chunk_offset += chunk.bytes();
                }

                let ResolvedType::Union(union_type) = r#type else {
                    unreachable!("a Union constant's own type is always ResolvedType::Union");
                };
                let field_type = union_type.borrow().fields[0].r#type.clone();
                let values = self.emit_const_value(builder, value, &field_type);
                self.store_scalars(builder, &PlaceStorage::Slot { slot, offset: 0 }, &values);

                self.load_scalars(builder, &PlaceStorage::Slot { slot, offset: 0 }, r#type)
            }
            // Materialize address-taken compile-time values in storage before taking their address.
            ConstValue::Ref(inner) => {
                let ResolvedType::Pointer { pointee, .. } = r#type else {
                    unreachable!("a Ref constant's own type is always ResolvedType::Pointer");
                };
                let inner_type = ref_pointee_type(inner, pointee);
                let data_id = self.build_const_data(inner, &inner_type);
                let global_value = self.module.declare_data_in_func(data_id, builder.func);
                vec![
                    builder
                        .ins()
                        .global_value(self.pointer_type(), global_value),
                ]
            }
        }
    }

    fn emit_const_slice(
        &mut self,
        builder: &mut FunctionBuilder,
        elements: &[ConstValue],
        item_type: &ResolvedType,
    ) -> Vec<Value> {
        let ptr_type = self.pointer_type();
        let len = builder.ins().iconst(types::I32, elements.len() as i64);
        let data_id = self.build_const_slice_data(elements, item_type);
        let global_value = self.module.declare_data_in_func(data_id, builder.func);
        let ptr = builder.ins().global_value(ptr_type, global_value);
        vec![ptr, len]
    }

    fn build_const_slice_data(
        &mut self,
        elements: &[ConstValue],
        item_type: &ResolvedType,
    ) -> DataId {
        let mut hash_input = Vec::new();
        for element in elements {
            self.hash_const_element(&mut hash_input, element, item_type);
        }
        let sym = Self::data_symbol(&hash_input);
        if let Some(id) = self.const_blobs.get(&sym) {
            return *id;
        }

        let stride = layout::total_bytes(item_type, self.pointer_bytes());
        let mut bytes = vec![0u8; stride as usize * elements.len()];
        let mut desc = DataDescription::new();
        for (i, element) in elements.iter().enumerate() {
            self.write_const_element(&mut desc, &mut bytes, i as u32 * stride, element, item_type);
        }
        desc.define(bytes.into_boxed_slice());

        let id = self
            .module
            .declare_data(&sym, cranelift_module::Linkage::Preemptible, false, false)
            .unwrap();
        self.module.define_data(id, &desc).unwrap();
        self.const_blobs.insert(sym, id);
        id
    }

    fn build_const_data(&mut self, value: &ConstValue, r#type: &ResolvedType) -> DataId {
        let mut hash_input = Vec::new();
        self.hash_const_element(&mut hash_input, value, r#type);
        let sym = Self::data_symbol(&hash_input);
        if let Some(id) = self.const_blobs.get(&sym) {
            return *id;
        }

        let total = layout::total_bytes(r#type, self.pointer_bytes());
        let mut bytes = vec![0u8; total as usize];
        let mut desc = DataDescription::new();
        self.write_const_element(&mut desc, &mut bytes, 0, value, r#type);
        desc.define(bytes.into_boxed_slice());

        let id = self
            .module
            .declare_data(&sym, cranelift_module::Linkage::Preemptible, false, false)
            .unwrap();
        self.module.define_data(id, &desc).unwrap();
        self.const_blobs.insert(sym, id);
        id
    }

    fn hash_const_element(&mut self, out: &mut Vec<u8>, value: &ConstValue, r#type: &ResolvedType) {
        match value {
            ConstValue::Number(number) => {
                let leaf_bytes = r#type.cranelift_leaves(self)[0].bytes();
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
                    unreachable!(
                        "mir body guarantees a nested Array constant's own type is SizedArray"
                    );
                };
                for element in elements {
                    self.hash_const_element(out, element, item);
                }
            }
            ConstValue::Struct(fields) => {
                let ResolvedType::Struct(struct_type) = r#type else {
                    unreachable!("a Struct constant's own type is always ResolvedType::Struct");
                };
                let field_types: Vec<ResolvedType> = struct_type
                    .borrow()
                    .fields
                    .iter()
                    .map(|field| field.r#type.clone())
                    .collect();
                for (value, field_type) in fields.iter().zip(&field_types) {
                    self.hash_const_element(out, value, field_type);
                }
            }
            ConstValue::Enum {
                variant_index,
                tag,
                dynamic_fields,
                fields,
                ..
            } => {
                let ResolvedType::Enum { cell, .. } = r#type else {
                    unreachable!("an Enum constant's own type is always ResolvedType::Enum");
                };
                out.extend_from_slice(&(*variant_index as u32).to_le_bytes());
                let tag_bits: u64 = match tag {
                    NumberValue::Signed(v) => *v as u64,
                    NumberValue::Unsigned(v) => *v,
                    NumberValue::Float(v) => v.to_bits(),
                };
                out.extend_from_slice(&tag_bits.to_le_bytes());
                let (dynamic_types, field_types) = {
                    let enum_type = cell.borrow();
                    let dynamic_types: Vec<ResolvedType> = enum_type
                        .dynamic_fields
                        .iter()
                        .map(|field| field.r#type.clone())
                        .collect();
                    let field_types: Vec<ResolvedType> = enum_type.variants[*variant_index]
                        .fields
                        .iter()
                        .map(|field| field.r#type.clone())
                        .collect();
                    (dynamic_types, field_types)
                };
                for (value, field_type) in dynamic_fields.iter().zip(&dynamic_types) {
                    self.hash_const_element(out, value, field_type);
                }
                for (value, field_type) in fields.iter().zip(&field_types) {
                    self.hash_const_element(out, value, field_type);
                }
            }
            ConstValue::Union { field_index, value } => {
                let ResolvedType::Union(union_type) = r#type else {
                    unreachable!("a Union constant's own type is always ResolvedType::Union");
                };
                out.extend_from_slice(&(*field_index as u32).to_le_bytes());
                let field_type = union_type.borrow().fields[*field_index].r#type.clone();
                self.hash_const_element(out, value, &field_type);
            }
            ConstValue::Ref(inner) => {
                let ResolvedType::Pointer { pointee, .. } = r#type else {
                    unreachable!("a Ref constant's own type is always ResolvedType::Pointer");
                };
                self.hash_const_element(out, inner, &ref_pointee_type(inner, pointee));
            }
        }
    }

    pub(super) fn write_const_element(
        &mut self,
        desc: &mut DataDescription,
        bytes: &mut [u8],
        offset: u32,
        value: &ConstValue,
        r#type: &ResolvedType,
    ) {
        match value {
            ConstValue::Number(number) => {
                let leaf_bytes = r#type.cranelift_leaves(self)[0].bytes();
                let raw: u64 = match number {
                    NumberValue::Signed(v) => *v as u64,
                    NumberValue::Unsigned(v) => *v,
                    NumberValue::Float(v) if leaf_bytes == 4 => (*v as f32).to_bits() as u64,
                    NumberValue::Float(v) => v.to_bits(),
                };
                let start = offset as usize;
                bytes[start..start + leaf_bytes as usize]
                    .copy_from_slice(&raw.to_le_bytes()[..leaf_bytes as usize]);
            }
            ConstValue::Bool(b) => bytes[offset as usize] = *b as u8,
            ConstValue::Char(c) => {
                let start = offset as usize;
                bytes[start..start + 4].copy_from_slice(&(*c as u32).to_le_bytes());
            }
            ConstValue::Str(s) => {
                let str_id = if let Some(id) = self.bytes.get(s) {
                    *id
                } else {
                    self.get_or_declare_global_bytes(s.clone())
                };
                let global_value = self.module.declare_data_in_data(str_id, desc);
                desc.write_data_addr(offset, global_value, 0);

                // `*str` carries data and length; do not assume null termination.
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
                bytes[len_start..len_start + 4]
                    .copy_from_slice(&(nested.len() as i32).to_le_bytes());
            }
            // Sized arrays are inline aggregates, unlike slice/string pointer-based storage.
            ConstValue::Array(elements) => {
                let ResolvedType::SizedArray(item, _) = r#type else {
                    unreachable!(
                        "mir body guarantees a nested Array constant's own type is SizedArray"
                    );
                };
                let stride = layout::total_bytes(item, self.pointer_bytes());
                for (i, element) in elements.iter().enumerate() {
                    self.write_const_element(
                        desc,
                        bytes,
                        offset + i as u32 * stride,
                        element,
                        item,
                    );
                }
            }
            // Use shared byte offsets for memory-backed aggregate construction, not leaf indices.
            ConstValue::Struct(fields) => {
                let ResolvedType::Struct(struct_type) = r#type else {
                    unreachable!("a Struct constant's own type is always ResolvedType::Struct");
                };
                let pointer_bytes = self.pointer_bytes();
                let field_types: Vec<ResolvedType> = struct_type
                    .borrow()
                    .fields
                    .iter()
                    .map(|field| field.r#type.clone())
                    .collect();
                for (i, (value, field_type)) in fields.iter().zip(&field_types).enumerate() {
                    let field_offset =
                        layout::field_byte_offset(&struct_type.borrow(), i, pointer_bytes);
                    self.write_const_element(desc, bytes, offset + field_offset, value, field_type);
                }
            }
            // Emit enum prefix first, then dynamic fields and payload at shared offsets.
            ConstValue::Enum {
                variant_index,
                tag,
                dynamic_fields,
                fields,
                ..
            } => {
                let ResolvedType::Enum { cell, .. } = r#type else {
                    unreachable!("an Enum constant's own type is always ResolvedType::Enum");
                };
                let pointer_bytes = self.pointer_bytes();
                let (tag_type, header, dynamic, body) = {
                    let enum_type = cell.borrow();
                    let variant = &enum_type.variants[*variant_index];
                    let header: Vec<(ResolvedType, ConstValue)> = enum_type
                        .header
                        .iter()
                        .zip(&variant.header_values)
                        .map(|(field, value)| (field.r#type.clone(), value.clone()))
                        .collect();
                    let dynamic: Vec<(u32, ResolvedType)> = (0..enum_type.dynamic_fields.len())
                        .map(|i| {
                            let field_offset =
                                layout::enum_dynamic_field_offset(&enum_type, i, pointer_bytes);
                            (field_offset, enum_type.dynamic_fields[i].r#type.clone())
                        })
                        .collect();
                    let body: Vec<(u32, ResolvedType)> = (0..variant.fields.len())
                        .map(|i| {
                            let field_offset = layout::enum_body_field_offset(
                                &enum_type,
                                *variant_index,
                                i,
                                pointer_bytes,
                            );
                            (field_offset, variant.fields[i].r#type.clone())
                        })
                        .collect();
                    (enum_type.tag_type.clone(), header, dynamic, body)
                };

                self.write_const_element(desc, bytes, offset, &ConstValue::Number(*tag), &tag_type);
                let mut header_offset = offset + layout::total_bytes(&tag_type, pointer_bytes);
                for (field_type, value) in &header {
                    self.write_const_element(desc, bytes, header_offset, value, field_type);
                    header_offset += layout::total_bytes(field_type, pointer_bytes);
                }
                for (value, (field_offset, field_type)) in dynamic_fields.iter().zip(&dynamic) {
                    self.write_const_element(desc, bytes, offset + field_offset, value, field_type);
                }
                for (value, (field_offset, field_type)) in fields.iter().zip(&body) {
                    self.write_const_element(desc, bytes, offset + field_offset, value, field_type);
                }
            }
            // Union construction stores the active field at offset zero with no tag.
            ConstValue::Union { field_index, value } => {
                let ResolvedType::Union(union_type) = r#type else {
                    unreachable!("a Union constant's own type is always ResolvedType::Union");
                };
                let field_type = union_type.borrow().fields[*field_index].r#type.clone();
                self.write_const_element(desc, bytes, offset, value, &field_type);
            }
            // Materialize address-taken compile-time values in storage before taking their address.
            ConstValue::Ref(inner) => {
                let ResolvedType::Pointer { pointee, .. } = r#type else {
                    unreachable!("a Ref constant's own type is always ResolvedType::Pointer");
                };
                let inner_type = ref_pointee_type(inner, pointee);
                let inner_id = self.build_const_data(inner, &inner_type);
                let global_value = self.module.declare_data_in_data(inner_id, desc);
                desc.write_data_addr(offset, global_value, 0);
            }
        }
    }

}
