
use super::Codegen;
use super::leaf::IntoCraneliftLeaves;
use super::place::PlaceStorage;
use omega_analyzer::layout;
use cranelift::codegen::ir::{FuncRef, Inst, StackSlot};
use cranelift::prelude::{
    AbiParam, FloatCC, FunctionBuilder, InstBuilder, IntCC, MemFlags, StackSlotData, StackSlotKind, Value, types,
};
use cranelift_module::{DataDescription, DataId, FuncId, Module};
use omega_analyzer::checked::{CastKind, NumberValue};
use omega_analyzer::resolved_type::{ConstValue, NumericKind, ResolvedFunctionType, ResolvedType};
use omega_hir::BinaryOp;
use omega_mir::{
    MirAddressOf, MirArrayLiteral, MirAssignment, MirBinaryOp, MirCast, MirDynamicCall, MirEnumConstruct, MirExpr,
    MirExprNode, MirFunctionCall, MirSlice, MirSpecCoerce, MirStructLiteral, MirUnionConstruct,
};

fn ref_pointee_type(inner: &ConstValue, leaf_type: &ResolvedType) -> ResolvedType {
    match inner {
        ConstValue::Array(elements) => {
            ResolvedType::SizedArray(Box::new(leaf_type.clone()), elements.len() as u32)
        }
        _ => leaf_type.clone(),
    }
}

impl Codegen {
    fn emit_bytes(&mut self, builder: &mut FunctionBuilder, s: String) -> Vec<Value> {
        let len = builder.ins().iconst(types::I32, s.len() as i64);

        let ptr_type = self.pointer_type();
        let data_id = if let Some(id) = self.bytes.get(&s) { *id } else { self.get_or_declare_global_bytes(s.clone()) };

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
        let id = self.module.declare_data(&sym, cranelift_module::Linkage::Preemptible, false, false).unwrap();

        let mut desc = DataDescription::new();
        desc.define(bytes.into_boxed_slice());
        self.module.define_data(id, &desc).unwrap();

        self.bytes.insert(s, id);

        id
    }

    pub(super) fn get_func_ref_from_id(&mut self, builder: &mut FunctionBuilder, func_id: FuncId) -> FuncRef {
        self.module.declare_func_in_func(func_id, builder.func)
    }

    fn emit_const_value(&mut self, builder: &mut FunctionBuilder, value: &ConstValue, r#type: &ResolvedType) -> Vec<Value> {
        match value {
            ConstValue::Number(number) => {
                let leaf = r#type.cranelift_leaves(self)[0];
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
            // Materialize compile-time aggregate fields in declared layout order.
            ConstValue::Struct(fields) => {
                let ResolvedType::Struct(struct_type) = r#type else {
                    unreachable!("a Struct constant's own type is always ResolvedType::Struct");
                };
                let field_types: Vec<ResolvedType> =
                    struct_type.borrow().fields.iter().map(|(_, t, _)| t.clone()).collect();
                fields
                    .iter()
                    .zip(&field_types)
                    .flat_map(|(value, field_type)| self.emit_const_value(builder, value, field_type))
                    .collect()
            }
            // Materialize enum tag/prefix/payload according to shared enum layout.
            ConstValue::Enum { variant_index, tag, dynamic_fields, fields, .. } => {
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
                        .map(|((_, t, _), v)| (t.clone(), v.clone()))
                        .collect();
                    let dynamic: Vec<(u32, ResolvedType)> = (0..enum_type.dynamic_fields.len())
                        .map(|i| {
                            let offset = layout::enum_dynamic_field_offset(&enum_type, i, pointer_bytes);
                            (offset, enum_type.dynamic_fields[i].1.clone())
                        })
                        .collect();
                    let body: Vec<(u32, ResolvedType)> = (0..variant.fields.len())
                        .map(|i| {
                            let offset = layout::enum_body_field_offset(&enum_type, *variant_index, i, pointer_bytes);
                            (offset, variant.fields[i].1.clone())
                        })
                        .collect();
                    (enum_type.tag_type.clone(), header, dynamic, body)
                };

                let shift = layout::stack_align_shift(layout::type_alignment(r#type));
                let total = layout::total_bytes(r#type, pointer_bytes);
                let slot = builder.create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, total, shift));

                let tag_values = self.emit_const_value(builder, &ConstValue::Number(*tag), &tag_type);
                self.store_scalars(builder, &PlaceStorage::Slot { slot, offset: 0 }, &tag_values);

                let mut offset = layout::total_bytes(&tag_type, pointer_bytes);
                for (field_type, value) in &header {
                    let values = self.emit_const_value(builder, value, field_type);
                    self.store_scalars(builder, &PlaceStorage::Slot { slot, offset }, &values);
                    offset += layout::total_bytes(field_type, pointer_bytes);
                }

                for (value, (field_offset, field_type)) in dynamic_fields.iter().zip(&dynamic) {
                    let values = self.emit_const_value(builder, value, field_type);
                    self.store_scalars(builder, &PlaceStorage::Slot { slot, offset: *field_offset }, &values);
                }

                for (value, (field_offset, field_type)) in fields.iter().zip(&body) {
                    let values = self.emit_const_value(builder, value, field_type);
                    self.store_scalars(builder, &PlaceStorage::Slot { slot, offset: *field_offset }, &values);
                }

                self.load_scalars(builder, &PlaceStorage::Slot { slot, offset: 0 }, r#type)
            }
            // Materialize the active union field at offset zero in shared union storage.
            ConstValue::Union { value, .. } => {
                let total = layout::total_bytes(r#type, self.pointer_bytes());
                let slot = builder.create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, total, 4));

                let mut chunk_offset = 0u32;
                for chunk in r#type.cranelift_leaves(self) {
                    let zero = builder.ins().iconst(chunk, 0);
                    builder.ins().stack_store(zero, slot, chunk_offset as i32);
                    chunk_offset += chunk.bytes();
                }

                let ResolvedType::Union(union_type) = r#type else {
                    unreachable!("a Union constant's own type is always ResolvedType::Union");
                };
                let field_type = union_type.borrow().fields[0].1.clone();
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
                vec![builder.ins().global_value(self.pointer_type(), global_value)]
            }
        }
    }

    fn emit_const_slice(&mut self, builder: &mut FunctionBuilder, elements: &[ConstValue], item_type: &ResolvedType) -> Vec<Value> {
        let ptr_type = self.pointer_type();
        let len = builder.ins().iconst(types::I32, elements.len() as i64);
        let data_id = self.build_const_slice_data(elements, item_type);
        let global_value = self.module.declare_data_in_func(data_id, builder.func);
        let ptr = builder.ins().global_value(ptr_type, global_value);
        vec![ptr, len]
    }

    fn build_const_slice_data(&mut self, elements: &[ConstValue], item_type: &ResolvedType) -> DataId {
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

        let id = self.module.declare_data(&sym, cranelift_module::Linkage::Preemptible, false, false).unwrap();
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

        let id = self.module.declare_data(&sym, cranelift_module::Linkage::Preemptible, false, false).unwrap();
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
                    unreachable!("mir body guarantees a nested Array constant's own type is SizedArray");
                };
                for element in elements {
                    self.hash_const_element(out, element, item);
                }
            }
            ConstValue::Struct(fields) => {
                let ResolvedType::Struct(struct_type) = r#type else {
                    unreachable!("a Struct constant's own type is always ResolvedType::Struct");
                };
                let field_types: Vec<ResolvedType> =
                    struct_type.borrow().fields.iter().map(|(_, t, _)| t.clone()).collect();
                for (value, field_type) in fields.iter().zip(&field_types) {
                    self.hash_const_element(out, value, field_type);
                }
            }
            ConstValue::Enum { variant_index, tag, dynamic_fields, fields, .. } => {
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
                    let dynamic_types: Vec<ResolvedType> =
                        enum_type.dynamic_fields.iter().map(|(_, t, _)| t.clone()).collect();
                    let field_types: Vec<ResolvedType> =
                        enum_type.variants[*variant_index].fields.iter().map(|(_, t, _)| t.clone()).collect();
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
                let field_type = union_type.borrow().fields[*field_index].1.clone();
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
                bytes[len_start..len_start + 4].copy_from_slice(&(nested.len() as i32).to_le_bytes());
            }
            // Sized arrays are inline aggregates, unlike slice/string pointer-based storage.
            ConstValue::Array(elements) => {
                let ResolvedType::SizedArray(item, _) = r#type else {
                    unreachable!("mir body guarantees a nested Array constant's own type is SizedArray");
                };
                let stride = layout::total_bytes(item, self.pointer_bytes());
                for (i, element) in elements.iter().enumerate() {
                    self.write_const_element(desc, bytes, offset + i as u32 * stride, element, item);
                }
            }
            // Use shared byte offsets for memory-backed aggregate construction, not leaf indices.
            ConstValue::Struct(fields) => {
                let ResolvedType::Struct(struct_type) = r#type else {
                    unreachable!("a Struct constant's own type is always ResolvedType::Struct");
                };
                let pointer_bytes = self.pointer_bytes();
                let field_types: Vec<ResolvedType> =
                    struct_type.borrow().fields.iter().map(|(_, t, _)| t.clone()).collect();
                for (i, (value, field_type)) in fields.iter().zip(&field_types).enumerate() {
                    let field_offset = layout::field_byte_offset(&struct_type.borrow(), i, pointer_bytes);
                    self.write_const_element(desc, bytes, offset + field_offset, value, field_type);
                }
            }
            // Emit enum prefix first, then dynamic fields and payload at shared offsets.
            ConstValue::Enum { variant_index, tag, dynamic_fields, fields, .. } => {
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
                        .map(|((_, t, _), v)| (t.clone(), v.clone()))
                        .collect();
                    let dynamic: Vec<(u32, ResolvedType)> = (0..enum_type.dynamic_fields.len())
                        .map(|i| {
                            let field_offset = layout::enum_dynamic_field_offset(&enum_type, i, pointer_bytes);
                            (field_offset, enum_type.dynamic_fields[i].1.clone())
                        })
                        .collect();
                    let body: Vec<(u32, ResolvedType)> = (0..variant.fields.len())
                        .map(|i| {
                            let field_offset = layout::enum_body_field_offset(&enum_type, *variant_index, i, pointer_bytes);
                            (field_offset, variant.fields[i].1.clone())
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
                let field_type = union_type.borrow().fields[*field_index].1.clone();
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

    fn promote_variadic_arg(&mut self, builder: &mut FunctionBuilder, value: Value, arg_type: &ResolvedType) -> Value {
        // C variadic promotion is shared; this backend only translates the promoted type.
        match crate::abi::variadic_promotion(arg_type, self.target) {
            Some(NumericKind::Float(_)) => builder.ins().fpromote(types::F64, value),
            Some(NumericKind::Signed(_)) => builder.ins().sextend(types::I32, value),
            Some(NumericKind::Unsigned(_)) => builder.ins().uextend(types::I32, value),
            None => value,
        }
    }

    fn maybe_sret_arg(
        &mut self,
        builder: &mut FunctionBuilder,
        fn_type: &ResolvedFunctionType,
        ir_args: &mut Vec<Value>,
    ) -> Option<StackSlot> {
        self.needs_sret(&fn_type.return_type).then(|| {
            let shift = layout::stack_align_shift(layout::type_alignment(&fn_type.return_type));
            let size = layout::total_bytes(&fn_type.return_type, self.pointer_bytes());
            let slot = builder.create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, size, shift));
            let pointer = builder.ins().stack_addr(self.pointer_type(), slot, 0);
            ir_args.insert(0, pointer);
            slot
        })
    }

    fn emit_call_indirect(
        &mut self,
        builder: &mut FunctionBuilder,
        fnaddr: Value,
        fn_type: &ResolvedFunctionType,
        ir_args: &[Value],
    ) -> Inst {
        // Cranelift variadic calls use a fixed signature synthesized for the concrete call site.
        let mut sig = self.make_function_sig(fn_type.clone());
        if fn_type.is_variadic && ir_args.len() > sig.params.len() {
            for arg in &ir_args[sig.params.len()..] {
                sig.params.push(AbiParam::new(builder.func.dfg.value_type(*arg)));
            }
        }
        let sigref = builder.import_signature(sig);
        builder.ins().call_indirect(sigref, fnaddr, ir_args)
    }

    fn call_result(
        &mut self,
        builder: &mut FunctionBuilder,
        fn_type: &ResolvedFunctionType,
        sret_slot: Option<StackSlot>,
        call: Inst,
    ) -> Vec<Value> {
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

    pub(super) fn process_expr(&mut self, builder: &mut FunctionBuilder, node: MirExprNode) -> Vec<Value> {
        match node.kind {
            MirExpr::String(s) => self.emit_bytes(builder, s),
            MirExpr::ByteString(s) => self.emit_bytes(builder, s),
            MirExpr::Const(value) => self.emit_const_value(builder, &value, &node.r#type),

            MirExpr::FunctionCall(MirFunctionCall { callee, fn_type, args }) => {
                // MIR guarantees a single resolved callee; codegen performs no overload selection.
                let fnaddr = self.process_expr(builder, *callee)[0];

                let fixed_count = fn_type.params.len();
                let mut ir_args = vec![];
                for (i, arg) in args.into_iter().enumerate() {
                    let arg_type = arg.r#type.clone();
                    let mut value = self.process_expr(builder, arg);
                    if fn_type.is_variadic && i >= fixed_count && let [v] = value.as_mut_slice() {
                        *v = self.promote_variadic_arg(builder, *v, &arg_type);
                    }
                    ir_args.push(value);
                }
                let mut ir_args = ir_args.into_iter().flatten().collect::<Vec<_>>();

                let sret_slot = self.maybe_sret_arg(builder, &fn_type, &mut ir_args);
                let call = self.emit_call_indirect(builder, fnaddr, &fn_type, &ir_args);
                self.call_result(builder, &fn_type, sret_slot, call)
            }

            // Dynamic-spec coercion builds the data-pointer/vtable-pointer pair.
            MirExpr::SpecCoerce(MirSpecCoerce { base, slots }) => {
                let ResolvedType::SpecObject { spec, type_args, .. } = &node.r#type else {
                    unreachable!("mir body guarantees a SpecCoerce's own type is SpecObject");
                };
                let spec = spec.clone();
                let type_args = type_args.clone();
                let ResolvedType::Pointer { pointee, .. } = &base.r#type else {
                    unreachable!("mir body guarantees a SpecCoerce's base is a plain pointer");
                };
                let concrete = (**pointee).clone();
                let data_ptr = self.process_expr(builder, *base)[0];
                let vtable_id = self.vtable_for(&concrete, &spec, &type_args, &slots);
                let global_value = self.module.declare_data_in_func(vtable_id, builder.func);
                let vtable_ptr = builder.ins().global_value(self.pointer_type(), global_value);
                vec![data_ptr, vtable_ptr]
            }

            // Dynamic-spec calls load the resolved slot from the vtable and call it indirectly.
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

                let sret_slot = self.maybe_sret_arg(builder, &fn_type, &mut ir_args);
                let call = self.emit_call_indirect(builder, fnaddr, &fn_type, &ir_args);
                self.call_result(builder, &fn_type, sret_slot, call)
            }

            MirExpr::Number(value) => {
                // This path requires a scalar leaf; aggregate shapes were rejected/resolved earlier.
                let ir_type = node.r#type.cranelift_leaves(self)[0];
                let result = match value {
                    NumberValue::Signed(v) => builder.ins().iconst(ir_type, v),
                    NumberValue::Unsigned(v) => builder.ins().iconst(ir_type, v as i64),
                    NumberValue::Float(v) if ir_type == types::F32 => builder.ins().f32const(v as f32),
                    NumberValue::Float(v) => builder.ins().f64const(v),
                };
                vec![result]
            }

            MirExpr::Bool(b) => vec![builder.ins().iconst(types::I8, b as i64)],

            // `sizeof` is emitted as a target-sized compile-time integer constant.
            MirExpr::Sizeof(target_type) => {
                let size = layout::total_bytes(&target_type, self.pointer_bytes());
                vec![builder.ins().iconst(self.pointer_type(), size as i64)]
            }

            // Represent `char` as its 32-bit integer leaf.
            MirExpr::Char(c) => vec![builder.ins().iconst(types::I32, c as i64)],

            MirExpr::Place(place) => self.get_place_value(&place, builder),

            MirExpr::Assignment(MirAssignment { target, value }) => {
                let values = self.process_expr(builder, *value);
                // All assignment destinations flow through the same resolved-place store path.
                let (storage, _) = self.resolve_place_storage(&target, builder);
                self.store_scalars(builder, &storage, &values);
                values
            }

            MirExpr::AddressOf(MirAddressOf { place }) => {
                let (storage, _) = self.resolve_place_storage(&place, builder);
                vec![self.place_storage_address(builder, &storage)]
            }

            MirExpr::Negate(base) => {
                let is_float = matches!(base.r#type.numeric_kind(self.pointer_bytes() * 8), Some(NumericKind::Float(_)));
                let value = self.process_expr(builder, *base)[0];
                let result = if is_float { builder.ins().fneg(value) } else { builder.ins().ineg(value) };
                vec![result]
            }

            MirExpr::BitNot(base) => {
                let value = self.process_expr(builder, *base)[0];
                vec![builder.ins().bnot(value)]
            }

            MirExpr::BinaryOp(MirBinaryOp { op, left, right }) => {
                let kind = match &left.r#type {
                    ResolvedType::Char => NumericKind::Unsigned(32),
                    ResolvedType::Bool => NumericKind::Unsigned(8),
                    r#type => r#type
                        .numeric_kind(self.pointer_bytes() * 8)
                        .expect("mir body guarantees BinaryOp operands are numeric, char, or bool"),
                };
                let left = self.process_expr(builder, *left)[0];
                let right = self.process_expr(builder, *right)[0];
                // Rely on backend integer div/rem traps; semantic checks need not duplicate them here.
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
                // Flatten array elements in source order into the aggregate leaf sequence.
                elements.into_iter().flat_map(|e| self.process_expr(builder, e)).collect()
            }

            MirExpr::EnumConstruct(MirEnumConstruct { variant_index, fields }) => {
                // Build enums in scratch storage because payload layout is byte-addressed, not purely leaf-addressed.
                let ResolvedType::Enum { cell, .. } = &node.r#type else {
                    unreachable!("mir body guarantees a construction's own type is its enum");
                };
                let cell = cell.clone();
                let pointer_bytes = self.pointer_bytes();
                // Copy layout facts before mutation to avoid holding the metadata-cell borrow.
                let (tag, tag_type, header, payload_offset, chunk_leaves, field_offsets) = {
                    let enum_type = cell.borrow();
                    let variant = &enum_type.variants[variant_index];
                    let header: Vec<(ResolvedType, ConstValue)> = enum_type
                        .header
                        .iter()
                        .zip(&variant.header_values)
                        .map(|((_, r#type, _), value)| (r#type.clone(), value.clone()))
                        .collect();
                    let field_offsets: Vec<u32> = (0..enum_type.dynamic_fields.len())
                        .map(|i| layout::enum_dynamic_field_offset(&enum_type, i, pointer_bytes))
                        .chain(
                            (0..variant.fields.len())
                                .map(|i| layout::enum_body_field_offset(&enum_type, variant_index, i, pointer_bytes)),
                        )
                        .collect();
                    let payload_offset = layout::enum_payload_offset(&enum_type, pointer_bytes);
                    let chunk_leaves = layout::payload_chunks(layout::enum_payload_bytes(
                        &enum_type,
                        enum_type.layout.pack,
                        pointer_bytes,
                    ));
                    (variant.tag, enum_type.tag_type.clone(), header, payload_offset, chunk_leaves, field_offsets)
                };

                let shift = layout::stack_align_shift(layout::type_alignment(&node.r#type));
                let total = layout::total_bytes(&node.r#type, pointer_bytes);
                let slot = builder.create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, total, shift));

                let tag_values = self.emit_const_value(builder, &ConstValue::Number(tag), &tag_type);
                self.store_scalars(builder, &PlaceStorage::Slot { slot, offset: 0 }, &tag_values);

                let mut offset = layout::total_bytes(&tag_type, pointer_bytes);
                for (r#type, value) in &header {
                    let const_values = self.emit_const_value(builder, value, r#type);
                    self.store_scalars(builder, &PlaceStorage::Slot { slot, offset }, &const_values);
                    offset += layout::total_bytes(r#type, pointer_bytes);
                }

                let mut chunk_offset = payload_offset;
                for leaf in chunk_leaves {
                    let chunk = super::leaf::cranelift_type(leaf, self.pointer_type());
                    let zero = builder.ins().iconst(chunk, 0);
                    builder.ins().stack_store(zero, slot, chunk_offset as i32);
                    chunk_offset += leaf.bytes(pointer_bytes);
                }

                // Preserve source evaluation order even though fields are stored by resolved layout.
                for field in fields {
                    let field_offset = field_offsets[field.field_index];
                    let values = self.process_expr(builder, field.value);
                    self.store_scalars(builder, &PlaceStorage::Slot { slot, offset: field_offset }, &values);
                }

                self.load_scalars(builder, &PlaceStorage::Slot { slot, offset: 0 }, &node.r#type)
            }

            MirExpr::StructLiteral(MirStructLiteral { fields }) => {
                // Evaluate aggregate initializers in source order; layout order affects storage only.
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

                // Range slicing derives the new data pointer and length from the original slice pair.
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
                    ResolvedType::Array(_, _) => {
                        let leaves = self.load_scalars(builder, &storage, &base_type);
                        (leaves[0], builder.ins().iconst(types::I32, 0))
                    }
                    _ => unreachable!("mir body guarantees a slice's base is SizedArray/Slice/Str/Array"),
                };

                let elem_size = layout::total_bytes(&item_type, self.pointer_bytes()) as i64;

                let start_val = match start {
                    Some(e) => self.process_expr(builder, *e)[0],
                    None => builder.ins().iconst(types::I32, 0),
                };
                // Inclusive ranges add one element to the computed slice length.
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
                // Unsizing uses the source aggregate length, not a reconstructed backend guess.
                let unsize_len = match &base.r#type {
                    ResolvedType::Pointer { pointee, .. } => match pointee.as_ref() {
                        ResolvedType::SizedArray(_, size) => Some(*size),
                        _ => None,
                    },
                    _ => None,
                };
                // Reinterpretation preserves the full flattened value before rebuilding the destination shape.
                let base_leaves = self.process_expr(builder, *base);
                let target_ir = target_type.cranelift_leaves(self)[0];
                match kind {
                    CastKind::Reinterpret => base_leaves,
                    CastKind::DropLength => vec![base_leaves[0]],
                    CastKind::Unsize => {
                        let len = unsize_len.expect("mir body guarantees Unsize's base is Pointer{SizedArray}");
                        let len_val = builder.ins().iconst(types::I32, len as i64);
                        vec![base_leaves[0], len_val]
                    }
                    CastKind::IntExtend { signed: true } => vec![builder.ins().sextend(target_ir, base_leaves[0])],
                    CastKind::IntExtend { signed: false } => vec![builder.ins().uextend(target_ir, base_leaves[0])],
                    CastKind::IntTruncate => vec![builder.ins().ireduce(target_ir, base_leaves[0])],
                    CastKind::IntToFloat { signed: true } => vec![builder.ins().fcvt_from_sint(target_ir, base_leaves[0])],
                    CastKind::IntToFloat { signed: false } => vec![builder.ins().fcvt_from_uint(target_ir, base_leaves[0])],
                    CastKind::FloatToInt { signed: true } => vec![builder.ins().fcvt_to_sint_sat(target_ir, base_leaves[0])],
                    CastKind::FloatToInt { signed: false } => vec![builder.ins().fcvt_to_uint_sat(target_ir, base_leaves[0])],
                    CastKind::FloatExtend => vec![builder.ins().fpromote(target_ir, base_leaves[0])],
                    CastKind::FloatTruncate => vec![builder.ins().fdemote(target_ir, base_leaves[0])],
                    // Spec narrowing preserves the data pointer and swaps in the resolved narrower vtable.
                    CastKind::SpecNarrow { slot_offset } => {
                        let byte_offset = slot_offset as i64 * self.pointer_bytes() as i64;
                        let vtable = builder.ins().iadd_imm(base_leaves[1], byte_offset);
                        vec![base_leaves[0], vtable]
                    }
                }
            }

            MirExpr::UnionConstruct(MirUnionConstruct { field_index: _, value }) => {
                // Enum constants use the same scratch-storage layout path as runtime enum construction.
                let total = layout::total_bytes(&node.r#type, self.pointer_bytes());
                let slot = builder.create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, total, 4));

                let mut chunk_offset = 0u32;
                for chunk in node.r#type.cranelift_leaves(self) {
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
}
