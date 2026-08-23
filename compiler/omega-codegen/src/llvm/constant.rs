use super::Codegen;
use super::leaf;
use super::place::PlaceStorage;
use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicValueEnum, GlobalValue};
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

pub(super) struct ConstBlob<'ctx> {
    bytes: Vec<u8>,
    relocs: Vec<(u32, GlobalValue<'ctx>)>,
    pointer_bytes: u32,
}

impl<'ctx> Codegen<'ctx> {
    pub(super) fn emit_bytes(&mut self, s: String) -> Vec<BasicValueEnum<'ctx>> {
        let len = self.context.i32_type().const_int(s.len() as u64, false);
        let data = self.get_or_declare_global_bytes(s);
        vec![data.as_pointer_value().into(), len.into()]
    }

    fn get_or_declare_global_bytes(&mut self, s: String) -> GlobalValue<'ctx> {
        if let Some(global) = self.bytes.get(&s) {
            return *global;
        }
        let symbol = omega_mir::mangle::data_symbol(s.as_bytes());
        let bytes = s.clone().into_bytes();
        let array_ty = self.context.i8_type().array_type(bytes.len() as u32);
        let global = self.module.add_global(array_ty, None, &symbol);
        global.set_linkage(inkwell::module::Linkage::WeakODR);
        global.set_constant(true);
        let elems: Vec<inkwell::values::IntValue> = bytes
            .iter()
            .map(|b| self.context.i8_type().const_int(*b as u64, false))
            .collect();
        global.set_initializer(&self.context.i8_type().const_array(&elems));
        if self.target.os != omega_analyzer::Os::MacOs {
            global.set_section(Some(&format!(".rodata.{symbol}")));
        }
        self.bytes.insert(s, global);
        global
    }

    fn build_const_slice_data(
        &mut self,
        elements: &[ConstValue],
        item_type: &ResolvedType,
    ) -> GlobalValue<'ctx> {
        let mut hash_input = Vec::new();
        for element in elements {
            self.hash_const_element(&mut hash_input, element, item_type);
        }
        let symbol = omega_mir::mangle::data_symbol(&hash_input);
        if let Some(global) = self.const_blobs.get(&symbol) {
            return *global;
        }

        let stride = layout::total_bytes(item_type, self.pointer_bytes());
        let mut blob = ConstBlob {
            bytes: vec![0u8; stride as usize * elements.len()],
            relocs: Vec::new(),
            pointer_bytes: self.pointer_bytes(),
        };
        for (i, element) in elements.iter().enumerate() {
            self.write_const_element(&mut blob, i as u32 * stride, element, item_type);
        }
        let global = self.declare_blob(&symbol, &blob);
        self.const_blobs.insert(symbol, global);
        global
    }

    fn build_const_data(&mut self, value: &ConstValue, r#type: &ResolvedType) -> GlobalValue<'ctx> {
        let mut hash_input = Vec::new();
        self.hash_const_element(&mut hash_input, value, r#type);
        let symbol = omega_mir::mangle::data_symbol(&hash_input);
        if let Some(global) = self.const_blobs.get(&symbol) {
            return *global;
        }

        let total = layout::total_bytes(r#type, self.pointer_bytes());
        let mut blob = ConstBlob {
            bytes: vec![0u8; total as usize],
            relocs: Vec::new(),
            pointer_bytes: self.pointer_bytes(),
        };
        self.write_const_element(&mut blob, 0, value, r#type);
        let global = self.declare_blob(&symbol, &blob);
        self.const_blobs.insert(symbol, global);
        global
    }

    pub(super) fn build_const_blob(
        &mut self,
        value: &ConstValue,
        r#type: &ResolvedType,
    ) -> ConstBlob<'ctx> {
        let total = layout::total_bytes(r#type, self.pointer_bytes());
        let mut blob = ConstBlob {
            bytes: vec![0u8; total as usize],
            relocs: Vec::new(),
            pointer_bytes: self.pointer_bytes(),
        };
        self.write_const_element(&mut blob, 0, value, r#type);
        blob
    }

    pub(super) fn materialize_blob(
        &mut self,
        blob: &ConstBlob<'ctx>,
    ) -> (BasicTypeEnum<'ctx>, BasicValueEnum<'ctx>) {
        let total = blob.bytes.len() as u32;
        if blob.relocs.is_empty() {
            // Keep zero-sized constants zero-sized; do not invent storage bytes.
            let ty: BasicTypeEnum = self.context.i8_type().array_type(total).into();
            let bytes: Vec<inkwell::values::IntValue> = blob
                .bytes
                .iter()
                .map(|b| self.context.i8_type().const_int(*b as u64, false))
                .collect();
            return (ty, self.context.i8_type().const_array(&bytes).into());
        }

        let mut sorted = blob.relocs.clone();
        sorted.sort_by_key(|(offset, _)| *offset);
        let mut fields: Vec<BasicTypeEnum> = Vec::new();
        let mut values: Vec<BasicValueEnum> = Vec::new();
        let mut cursor = 0u32;
        for (offset, target) in &sorted {
            if *offset > cursor {
                let span: Vec<inkwell::values::IntValue> = blob.bytes
                    [cursor as usize..*offset as usize]
                    .iter()
                    .map(|b| self.context.i8_type().const_int(*b as u64, false))
                    .collect();
                fields.push(self.context.i8_type().array_type(span.len() as u32).into());
                values.push(self.context.i8_type().const_array(&span).into());
            }
            fields.push(self.ptr_type().into());
            values.push(target.as_pointer_value().into());
            cursor = offset + blob.pointer_bytes;
        }
        if cursor < total {
            let span: Vec<inkwell::values::IntValue> = blob.bytes[cursor as usize..]
                .iter()
                .map(|b| self.context.i8_type().const_int(*b as u64, false))
                .collect();
            fields.push(self.context.i8_type().array_type(span.len() as u32).into());
            values.push(self.context.i8_type().const_array(&span).into());
        }
        let struct_ty = self.context.struct_type(&fields, true);
        (
            struct_ty.into(),
            struct_ty.const_named_struct(&values).into(),
        )
    }

    fn declare_blob(&mut self, symbol: &str, blob: &ConstBlob<'ctx>) -> GlobalValue<'ctx> {
        let (ty, init) = self.materialize_blob(blob);
        let global = self.module.add_global(ty, None, symbol);
        global.set_linkage(inkwell::module::Linkage::WeakODR);
        global.set_constant(true);
        global.set_initializer(&init);
        global.set_alignment(1);
        if self.target.os != omega_analyzer::Os::MacOs {
            global.set_section(Some(&format!(".rodata.{symbol}")));
        }
        global
    }

    pub(super) fn emit_const_value(
        &mut self,
        value: &ConstValue,
        r#type: &ResolvedType,
    ) -> Vec<BasicValueEnum<'ctx>> {
        match value {
            ConstValue::Number(number) => {
                let raw_leaf = omega_analyzer::layout::leaves_of(r#type, self.pointer_bytes())[0];
                vec![self.scalar_const(raw_leaf, number)]
            }
            ConstValue::Bool(b) => vec![
                self.context
                    .i8_type()
                    .const_int(u64::from(*b), false)
                    .into(),
            ],
            ConstValue::Char(c) => vec![self.context.i32_type().const_int(*c as u64, false).into()],
            ConstValue::Str(s) => self.emit_bytes(s.clone()),
            ConstValue::Slice(elements) => {
                let item = match r#type {
                    ResolvedType::Slice { item, .. } => item,
                    _ => unreachable!("mir body guarantees a Slice constant's own type is Slice"),
                };
                let len = self
                    .context
                    .i32_type()
                    .const_int(elements.len() as u64, false);
                let data = self.build_const_slice_data(elements, item);
                vec![data.as_pointer_value().into(), len.into()]
            }
            ConstValue::Array(elements) => {
                let item = match r#type {
                    ResolvedType::SizedArray(item, _) => item,
                    _ => unreachable!(
                        "mir body guarantees an Array constant's own type is SizedArray"
                    ),
                };
                elements
                    .iter()
                    .flat_map(|element| self.emit_const_value(element, item))
                    .collect()
            }
            ConstValue::Struct(fields) => {
                let struct_type = match r#type {
                    ResolvedType::Struct(struct_type) => struct_type,
                    _ => {
                        unreachable!("a Struct constant's own type is always ResolvedType::Struct")
                    }
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
                    .flat_map(|(value, field_type)| self.emit_const_value(value, field_type))
                    .collect()
            }
            ConstValue::Enum {
                variant_index,
                tag,
                dynamic_fields,
                fields,
                ..
            } => {
                let cell = match r#type {
                    ResolvedType::Enum { cell, .. } => cell,
                    _ => unreachable!("an Enum constant's own type is always ResolvedType::Enum"),
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
                let slot = self.entry_alloca(total, 1u32 << shift, "enumconst");

                let tag_values = self.emit_const_value(&ConstValue::Number(*tag), &tag_type);
                self.store_scalars(&slot, 0, &tag_values, layout::type_alignment(&tag_type));

                let mut offset = layout::total_bytes(&tag_type, pointer_bytes);
                for (field_type, value) in &header {
                    let values = self.emit_const_value(value, field_type);
                    self.store_scalars(&slot, offset, &values, layout::type_alignment(field_type));
                    offset += layout::total_bytes(field_type, pointer_bytes);
                }

                for (value, (field_offset, field_type)) in dynamic_fields.iter().zip(&dynamic) {
                    let values = self.emit_const_value(value, field_type);
                    self.store_scalars(
                        &slot,
                        *field_offset,
                        &values,
                        layout::type_alignment(field_type),
                    );
                }

                for (value, (field_offset, field_type)) in fields.iter().zip(&body) {
                    let values = self.emit_const_value(value, field_type);
                    self.store_scalars(
                        &slot,
                        *field_offset,
                        &values,
                        layout::type_alignment(field_type),
                    );
                }

                self.load_scalars(
                    &PlaceStorage::Slot { slot, offset: 0 },
                    r#type,
                    layout::type_alignment(r#type),
                )
            }
            ConstValue::Union { value, .. } => {
                let total = layout::total_bytes(r#type, self.pointer_bytes());
                let slot = self.entry_alloca(total, 16, "unionconst");

                let mut chunk_offset = 0u32;
                for raw_leaf in omega_analyzer::layout::leaves_of(r#type, self.pointer_bytes()) {
                    let llvm_ty = leaf::llvm_type(self.context, raw_leaf, self.pointer_bytes());
                    let zero = match llvm_ty {
                        BasicTypeEnum::IntType(it) => it.const_zero().into(),
                        BasicTypeEnum::FloatType(ft) => ft.const_zero().into(),
                        BasicTypeEnum::PointerType(_) => self.ptr_type().const_null().into(),
                        _ => unreachable!("a union chunk is always a scalar"),
                    };
                    self.store_scalars(&slot, chunk_offset, &[zero], 1);
                    chunk_offset += raw_leaf.bytes(self.pointer_bytes());
                }

                let union_type = match r#type {
                    ResolvedType::Union(union_type) => union_type,
                    _ => unreachable!("a Union constant's own type is always ResolvedType::Union"),
                };
                let field_type = union_type.borrow().fields[0].r#type.clone();
                let values = self.emit_const_value(value, &field_type);
                self.store_scalars(&slot, 0, &values, layout::type_alignment(&field_type));

                self.load_scalars(
                    &PlaceStorage::Slot { slot, offset: 0 },
                    r#type,
                    layout::type_alignment(r#type),
                )
            }
            ConstValue::Ref(inner) => {
                let pointee = match r#type {
                    ResolvedType::Pointer { pointee, .. } => pointee,
                    _ => unreachable!("a Ref constant's own type is always ResolvedType::Pointer"),
                };
                let inner_type = ref_pointee_type(inner, pointee);
                let data = self.build_const_data(inner, &inner_type);
                vec![data.as_pointer_value().into()]
            }
        }
    }

    fn write_const_element(
        &mut self,
        blob: &mut ConstBlob<'ctx>,
        offset: u32,
        value: &ConstValue,
        r#type: &ResolvedType,
    ) {
        let pointer_bytes = self.pointer_bytes();
        match value {
            ConstValue::Number(number) => {
                let leaf = omega_analyzer::layout::leaves_of(r#type, pointer_bytes)[0];
                let leaf_bytes = leaf.bytes(pointer_bytes);
                let raw: u64 = match number {
                    NumberValue::Signed(v) => *v as u64,
                    NumberValue::Unsigned(v) => *v,
                    NumberValue::Float(v) if leaf_bytes == 4 => (*v as f32).to_bits() as u64,
                    NumberValue::Float(v) => v.to_bits(),
                };
                let start = offset as usize;
                blob.bytes[start..start + leaf_bytes as usize]
                    .copy_from_slice(&raw.to_le_bytes()[..leaf_bytes as usize]);
            }
            ConstValue::Bool(b) => blob.bytes[offset as usize] = *b as u8,
            ConstValue::Char(c) => {
                let start = offset as usize;
                blob.bytes[start..start + 4].copy_from_slice(&(*c as u32).to_le_bytes());
            }
            ConstValue::Str(s) => {
                let data = if let Some(global) = self.bytes.get(s) {
                    *global
                } else {
                    self.get_or_declare_global_bytes(s.clone())
                };
                blob.relocs.push((offset, data));
                let len_start = (offset + pointer_bytes) as usize;
                blob.bytes[len_start..len_start + 4]
                    .copy_from_slice(&(s.len() as i32).to_le_bytes());
            }
            ConstValue::Slice(nested) => {
                let item = match r#type {
                    ResolvedType::Slice { item, .. } => item,
                    _ => unreachable!(
                        "mir body guarantees a nested Slice constant's own type is Slice"
                    ),
                };
                let nested_id = self.build_const_slice_data(nested, item);
                blob.relocs.push((offset, nested_id));
                let len_start = (offset + pointer_bytes) as usize;
                blob.bytes[len_start..len_start + 4]
                    .copy_from_slice(&(nested.len() as i32).to_le_bytes());
            }
            ConstValue::Array(elements) => {
                let item = match r#type {
                    ResolvedType::SizedArray(item, _) => item,
                    _ => unreachable!(
                        "mir body guarantees a nested Array constant's own type is SizedArray"
                    ),
                };
                let stride = layout::total_bytes(item, pointer_bytes);
                for (i, element) in elements.iter().enumerate() {
                    self.write_const_element(blob, offset + i as u32 * stride, element, item);
                }
            }
            ConstValue::Struct(fields) => {
                let struct_type = match r#type {
                    ResolvedType::Struct(struct_type) => struct_type,
                    _ => {
                        unreachable!("a Struct constant's own type is always ResolvedType::Struct")
                    }
                };
                let field_types: Vec<ResolvedType> = struct_type
                    .borrow()
                    .fields
                    .iter()
                    .map(|field| field.r#type.clone())
                    .collect();
                for (i, (value, field_type)) in fields.iter().zip(&field_types).enumerate() {
                    let field_offset =
                        layout::field_byte_offset(&struct_type.borrow(), i, pointer_bytes);
                    self.write_const_element(blob, offset + field_offset, value, field_type);
                }
            }
            ConstValue::Enum {
                variant_index,
                tag,
                dynamic_fields,
                fields,
                ..
            } => {
                let cell = match r#type {
                    ResolvedType::Enum { cell, .. } => cell,
                    _ => unreachable!("an Enum constant's own type is always ResolvedType::Enum"),
                };
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

                self.write_const_element(blob, offset, &ConstValue::Number(*tag), &tag_type);
                let mut header_offset = offset + layout::total_bytes(&tag_type, pointer_bytes);
                for (field_type, value) in &header {
                    self.write_const_element(blob, header_offset, value, field_type);
                    header_offset += layout::total_bytes(field_type, pointer_bytes);
                }
                for (value, (field_offset, field_type)) in dynamic_fields.iter().zip(&dynamic) {
                    self.write_const_element(blob, offset + field_offset, value, field_type);
                }
                for (value, (field_offset, field_type)) in fields.iter().zip(&body) {
                    self.write_const_element(blob, offset + field_offset, value, field_type);
                }
            }
            ConstValue::Union { field_index, value } => {
                let union_type = match r#type {
                    ResolvedType::Union(union_type) => union_type,
                    _ => unreachable!("a Union constant's own type is always ResolvedType::Union"),
                };
                let field_type = union_type.borrow().fields[*field_index].r#type.clone();
                self.write_const_element(blob, offset, value, &field_type);
            }
            ConstValue::Ref(inner) => {
                let pointee = match r#type {
                    ResolvedType::Pointer { pointee, .. } => pointee,
                    _ => unreachable!("a Ref constant's own type is always ResolvedType::Pointer"),
                };
                let inner_type = ref_pointee_type(inner, pointee);
                let inner_id = self.build_const_data(inner, &inner_type);
                blob.relocs.push((offset, inner_id));
            }
        }
    }

    fn hash_const_element(&mut self, out: &mut Vec<u8>, value: &ConstValue, r#type: &ResolvedType) {
        let pointer_bytes = self.pointer_bytes();
        match value {
            ConstValue::Number(number) => {
                let leaf = omega_analyzer::layout::leaves_of(r#type, pointer_bytes)[0];
                let leaf_bytes = leaf.bytes(pointer_bytes);
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
                let item = match r#type {
                    ResolvedType::Slice { item, .. } => item,
                    _ => unreachable!(
                        "mir body guarantees a nested Slice constant's own type is Slice"
                    ),
                };
                out.extend_from_slice(&(nested.len() as u32).to_le_bytes());
                for element in nested {
                    self.hash_const_element(out, element, item);
                }
            }
            ConstValue::Array(elements) => {
                let item = match r#type {
                    ResolvedType::SizedArray(item, _) => item,
                    _ => unreachable!(
                        "mir body guarantees a nested Array constant's own type is SizedArray"
                    ),
                };
                for element in elements {
                    self.hash_const_element(out, element, item);
                }
            }
            ConstValue::Struct(fields) => {
                let struct_type = match r#type {
                    ResolvedType::Struct(struct_type) => struct_type,
                    _ => {
                        unreachable!("a Struct constant's own type is always ResolvedType::Struct")
                    }
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
                let cell = match r#type {
                    ResolvedType::Enum { cell, .. } => cell,
                    _ => unreachable!("an Enum constant's own type is always ResolvedType::Enum"),
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
                let union_type = match r#type {
                    ResolvedType::Union(union_type) => union_type,
                    _ => unreachable!("a Union constant's own type is always ResolvedType::Union"),
                };
                out.extend_from_slice(&(*field_index as u32).to_le_bytes());
                let field_type = union_type.borrow().fields[*field_index].r#type.clone();
                self.hash_const_element(out, value, &field_type);
            }
            ConstValue::Ref(inner) => {
                let pointee = match r#type {
                    ResolvedType::Pointer { pointee, .. } => pointee,
                    _ => unreachable!("a Ref constant's own type is always ResolvedType::Pointer"),
                };
                self.hash_const_element(out, inner, &ref_pointee_type(inner, pointee));
            }
        }
    }
}
