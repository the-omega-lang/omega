
use super::Codegen;
use super::leaf::IntoCraneliftLeaves;
use omega_analyzer::layout;
use cranelift::codegen::ir::StackSlot;
use cranelift::prelude::{FunctionBuilder, InstBuilder, MemFlags, StackSlotData, StackSlotKind, Value};
use cranelift_module::Module;
use omega_analyzer::resolved_type::ResolvedType;
use omega_mir::{MirPlace, MirPlaceRoot, MirProjection};

pub(super) enum PlaceStorage {
    Values(Vec<Value>),
    Slot { slot: StackSlot, offset: u32 },
    Address { base: Value, offset: u32 },
}

impl Codegen {
    pub(super) fn resolve_place_storage(
        &mut self,
        place: &MirPlace,
        builder: &mut FunctionBuilder,
    ) -> (PlaceStorage, ResolvedType) {
        let (mut current, mut current_type) = match &place.root {
            MirPlaceRoot::Local { id, r#type } => {
                let current = if (id.0 as usize) < self.arg_count {
                    PlaceStorage::Values(self.local_args[id.0 as usize].clone())
                } else {
                    // Use the shared frame slot plus precomputed local offset so zero-sized locals follow shared layout.
                    let slot = self.frame_slot.expect("define_function_def always sets this before any block runs");
                    PlaceStorage::Slot { slot, offset: self.local_offsets[id.0 as usize] }
                };
                (current, r#type.clone())
            }
            MirPlaceRoot::Function(_) => {
                unreachable!(
                    "a function reference is never itself further-projected; calls resolve it directly via get_place_value"
                );
            }
            // Treat globals as ordinary address-backed storage before applying projections.
            MirPlaceRoot::Global { id, r#type } => {
                let data_id = *self.globals.get(id).unwrap_or_else(|| {
                    panic!("mir body guarantees {id:?} was declared as a global before this use")
                });
                let global_value = self.module.declare_data_in_func(data_id, builder.func);
                let base = builder.ins().global_value(self.pointer_type(), global_value);
                (PlaceStorage::Address { base, offset: 0 }, r#type.clone())
            }
            // Spill temporary projection roots so their address and subfields remain addressable.
            MirPlaceRoot::Expr(expr) => {
                let r#type = expr.r#type.clone();
                let values = self.process_expr(builder, (**expr).clone());
                let shift = layout::stack_align_shift(layout::type_alignment(&r#type));
                let size = layout::total_bytes(&r#type, self.pointer_bytes());
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
                        PlaceStorage::Values(values) => PlaceStorage::Values(layout::project_field_access(
                            &values,
                            &struct_type,
                            *index,
                            self.pointer_bytes(),
                        )),
                        PlaceStorage::Slot { slot, offset } => PlaceStorage::Slot {
                            slot,
                            offset: offset + layout::field_byte_offset(&struct_type, *index, self.pointer_bytes()),
                        },
                        PlaceStorage::Address { base, offset } => PlaceStorage::Address {
                            base,
                            offset: offset + layout::field_byte_offset(&struct_type, *index, self.pointer_bytes()),
                        },
                    };
                    current_type = r#type.clone();
                }

                // Union projections reinterpret offset-zero storage; spill SSA-backed unions before projecting.
                MirProjection::UnionField { r#type, .. } => {
                    if let PlaceStorage::Values(values) = &current {
                        let shift = layout::stack_align_shift(layout::type_alignment(&current_type));
                        let size = layout::total_bytes(&current_type, self.pointer_bytes());
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
                    // Index scaling comes from the resolved element type, not the container leaf shape.
                    let element_ir_size = layout::total_bytes(item_type, self.pointer_bytes());

                    let mut base = match &current_type {
                        // Sized arrays index from inline storage; pointer-backed containers index from their data pointer.
                        ResolvedType::SizedArray(_, _) => self.place_storage_address(builder, &current),
                        // Unsized arrays use their pointer value; slices/strings use the first leaf as the data pointer.
                        ResolvedType::Array(_, _) | ResolvedType::Slice { .. } | ResolvedType::Str { .. } => {
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
                    // Enum tags begin at byte/leaf offset zero.
                    let ResolvedType::Enum { cell, .. } = &current_type else {
                        unreachable!("mir body guarantees EnumTag projections are only built against an enum type");
                    };
                    let tag_leaves = cell.borrow().tag_type.cranelift_leaves(self).len();
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
                            // Use shared enum-prefix layout offsets; leaf position alone cannot model alignment gaps.
                            let start = layout::enum_prefix_layout(&enum_type, self.pointer_bytes()).leaf_starts
                                [1 + *index];
                            let len = enum_type.header[*index].r#type.cranelift_leaves(self).len();
                            PlaceStorage::Values(values[start..start + len].to_vec())
                        }
                        PlaceStorage::Slot { slot, offset } => PlaceStorage::Slot {
                            slot,
                            offset: offset + layout::enum_header_offset(&enum_type, *index, self.pointer_bytes()),
                        },
                        PlaceStorage::Address { base, offset } => PlaceStorage::Address {
                            base,
                            offset: offset + layout::enum_header_offset(&enum_type, *index, self.pointer_bytes()),
                        },
                    };
                    current_type = r#type.clone();
                }

                // Dynamic enum fields use the same resolved-prefix offset logic as header fields.
                MirProjection::EnumDynamicField { index, r#type, .. } => {
                    let ResolvedType::Enum { cell, .. } = &current_type else {
                        unreachable!("mir body guarantees EnumDynamicField projections are only built against an enum type");
                    };
                    let cell = cell.clone();
                    let enum_type = cell.borrow();
                    current = match current {
                        PlaceStorage::Values(values) => {
                            let start = layout::enum_prefix_layout(&enum_type, self.pointer_bytes()).leaf_starts
                                [1 + enum_type.header.len() + *index];
                            let len = enum_type.dynamic_fields[*index].r#type.cranelift_leaves(self).len();
                            PlaceStorage::Values(values[start..start + len].to_vec())
                        }
                        PlaceStorage::Slot { slot, offset } => PlaceStorage::Slot {
                            slot,
                            offset: offset
                                + layout::enum_dynamic_field_offset(&enum_type, *index, self.pointer_bytes()),
                        },
                        PlaceStorage::Address { base, offset } => PlaceStorage::Address {
                            base,
                            offset: offset
                                + layout::enum_dynamic_field_offset(&enum_type, *index, self.pointer_bytes()),
                        },
                    };
                    current_type = r#type.clone();
                }

                MirProjection::EnumBody { variant_index, field_index, r#type } => {
                    let ResolvedType::Enum { cell, .. } = &current_type else {
                        unreachable!("mir body guarantees EnumBody projections are only built against an enum type");
                    };
                    let cell = cell.clone();
                    // Spill SSA-backed enums before payload projection because payload chunks are byte-addressed.
                    if let PlaceStorage::Values(values) = &current {
                        let shift = layout::stack_align_shift(layout::type_alignment(&current_type));
                        let size = layout::total_bytes(&current_type, self.pointer_bytes());
                        let slot = builder
                            .create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, size, shift));
                        let spilled = PlaceStorage::Slot { slot, offset: 0 };
                        self.store_scalars(builder, &spilled, &values.clone());
                        current = spilled;
                    }
                    let field_offset = layout::enum_body_field_offset(
                        &cell.borrow(),
                        *variant_index,
                        *field_index,
                        self.pointer_bytes(),
                    );
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
                    // Slice place access uses the data-pointer leaf; length is metadata.
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

                // Dynamic-spec place access uses the data-pointer leaf; vtable is metadata.
                MirProjection::SpecObjectPtr { mutable } => {
                    current = match current {
                        PlaceStorage::Values(values) => PlaceStorage::Values(vec![values[0]]),
                        other => other,
                    };
                    current_type = ResolvedType::Pointer { pointee: Box::new(ResolvedType::U8), mutable: *mutable };
                }
                MirProjection::SpecObjectVtable => {
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
                    current_type = ResolvedType::Pointer { pointee: Box::new(ResolvedType::U8), mutable: false };
                }
            }
        }

        (current, current_type)
    }

    pub(super) fn place_storage_address(&mut self, builder: &mut FunctionBuilder, storage: &PlaceStorage) -> Value {
        let ptr_type = self.pointer_type();
        match storage {
            // Spill address-taken parameters once and reuse the spill for subsequent projections.
            PlaceStorage::Values(values) => {
                let size: u32 = values.iter().map(|v| builder.func.dfg.value_type(*v).bytes()).sum();
                let slot = builder.create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, size, 4));
                let spilled = PlaceStorage::Slot { slot, offset: 0 };
                self.store_scalars(builder, &spilled, values);
                builder.ins().stack_addr(ptr_type, slot, 0)
            }
            PlaceStorage::Slot { slot, offset } => builder.ins().stack_addr(ptr_type, *slot, *offset as i32),
            PlaceStorage::Address { base, offset: 0 } => *base,
            PlaceStorage::Address { base, offset } => {
                let offset_val = builder.ins().iconst(ptr_type, *offset as i64);
                builder.ins().iadd(*base, offset_val)
            }
        }
    }

    pub(super) fn load_scalars(
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
        for leaf in r#type.cranelift_leaves(self) {
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

    pub(super) fn store_scalars(&mut self, builder: &mut FunctionBuilder, storage: &PlaceStorage, values: &[Value]) {
        let mut rel_offset = 0u32;
        for value in values {
            let leaf = builder.func.dfg.value_type(*value);
            match storage {
                PlaceStorage::Values(_) => {
                    unreachable!(
                        "assignment into a function parameter is rejected by the shared preflight (crate::preflight) before any backend runs"
                    );
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

    pub(super) fn get_place_value(&mut self, place: &MirPlace, builder: &mut FunctionBuilder) -> Vec<Value> {
        // Function references are code pointers and cannot be materialized as ordinary data places.
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
}
