use super::Codegen;
use super::leaf;
use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicValue, BasicValueEnum, PointerValue};
use omega_analyzer::layout;
use omega_analyzer::resolved_type::ResolvedType;
use omega_mir::{MirPlace, MirPlaceRoot, MirProjection};

#[derive(Clone)]
pub(super) enum PlaceStorage<'ctx> {
    Values(Vec<BasicValueEnum<'ctx>>),
    Slot {
        slot: PointerValue<'ctx>,
        offset: u32,
    },
    Address {
        base: PointerValue<'ctx>,
        offset: u32,
    },
}

impl<'ctx> Codegen<'ctx> {
    pub(super) fn resolve_place_storage(
        &mut self,
        place: &MirPlace,
    ) -> (PlaceStorage<'ctx>, ResolvedType, u32) {
        let (mut current, mut current_type) = match &place.root {
            MirPlaceRoot::Local { id, r#type } => {
                let current = if id.index() < self.arg_count {
                    PlaceStorage::Values(self.local_args[id.index()].clone())
                } else {
                    let slot = self.frame_slot.expect(
                        "define_function_def always sets this before any block runs (a zero-size \
                         frame still means a local's address is the frame's own base)",
                    );
                    PlaceStorage::Slot {
                        slot,
                        offset: self.local_offsets[id.index()],
                    }
                };
                (current, r#type.clone())
            }
            MirPlaceRoot::Function(_) => {
                unreachable!(
                    "a function reference is never itself further-projected; calls resolve it directly via get_place_value"
                );
            }
            MirPlaceRoot::Global { id, r#type } => {
                let global = *self.globals.get(id).unwrap_or_else(|| {
                    panic!("mir body guarantees {id:?} was declared as a global before this use")
                });
                let base = global.as_pointer_value();
                (PlaceStorage::Address { base, offset: 0 }, r#type.clone())
            }
            // Spill temporary projection roots so field/index/address operations have stable storage.
            MirPlaceRoot::Expr(expr) => {
                let r#type = expr.r#type.clone();
                let values = self.process_expr(expr);
                let shift = layout::stack_align_shift(layout::type_alignment(&r#type));
                let size = layout::total_bytes(&r#type, self.pointer_bytes());
                let slot = self.entry_alloca(size, 1u32 << shift, "tmp");
                let storage = PlaceStorage::Slot { slot, offset: 0 };
                self.store_scalars(&slot, 0, &values, layout::type_alignment(&r#type));
                (storage, r#type)
            }
        };

        for projection in &place.projections {
            match projection {
                MirProjection::FieldAccess { index, r#type, .. } => {
                    let ResolvedType::Struct(struct_type) = &current_type else {
                        unreachable!(
                            "mir body guarantees field projections are only built against a struct type"
                        );
                    };
                    let struct_type = struct_type.clone();
                    let struct_type = struct_type.borrow();
                    current = match current {
                        PlaceStorage::Values(values) => {
                            PlaceStorage::Values(layout::project_field_access(
                                &values,
                                &struct_type,
                                *index,
                                self.pointer_bytes(),
                            ))
                        }
                        PlaceStorage::Slot { slot, offset } => PlaceStorage::Slot {
                            slot,
                            offset: offset
                                + layout::field_byte_offset(
                                    &struct_type,
                                    *index,
                                    self.pointer_bytes(),
                                ),
                        },
                        PlaceStorage::Address { base, offset } => PlaceStorage::Address {
                            base,
                            offset: offset
                                + layout::field_byte_offset(
                                    &struct_type,
                                    *index,
                                    self.pointer_bytes(),
                                ),
                        },
                    };
                    current_type = r#type.clone();
                }

                MirProjection::UnionField { r#type, .. } => {
                    if let PlaceStorage::Values(values) = &current {
                        let shift =
                            layout::stack_align_shift(layout::type_alignment(&current_type));
                        let size = layout::total_bytes(&current_type, self.pointer_bytes());
                        let slot = self.entry_alloca(size, 1u32 << shift, "union_spill");
                        self.store_scalars(&slot, 0, values, layout::type_alignment(&current_type));
                        current = PlaceStorage::Slot { slot, offset: 0 };
                    }
                    current_type = r#type.clone();
                }

                MirProjection::Deref { r#type } => {
                    let ptr_value = self.load_scalars(
                        &match &current {
                            PlaceStorage::Slot { slot, offset } => PlaceStorage::Slot {
                                slot: *slot,
                                offset: *offset,
                            },
                            PlaceStorage::Address { base, offset } => PlaceStorage::Address {
                                base: *base,
                                offset: *offset,
                            },
                            PlaceStorage::Values(values) => PlaceStorage::Values(values.clone()),
                        },
                        &current_type,
                        layout::type_alignment(&current_type),
                    )[0]
                    .into_pointer_value();
                    current = PlaceStorage::Address {
                        base: ptr_value,
                        offset: 0,
                    };
                    current_type = r#type.clone();
                }

                MirProjection::Index {
                    index_expr,
                    item_type,
                } => {
                    let element_size = layout::total_bytes(item_type, self.pointer_bytes());

                    let mut base = match &current_type {
                        ResolvedType::SizedArray(_, _) => {
                            self.place_storage_address(&match &current {
                                PlaceStorage::Slot { slot, offset } => PlaceStorage::Slot {
                                    slot: *slot,
                                    offset: *offset,
                                },
                                PlaceStorage::Address { base, offset } => PlaceStorage::Address {
                                    base: *base,
                                    offset: *offset,
                                },
                                PlaceStorage::Values(values) => {
                                    PlaceStorage::Values(values.clone())
                                }
                            })
                        }
                        ResolvedType::Array(_, _)
                        | ResolvedType::Slice { .. }
                        | ResolvedType::Str { .. } => self.load_scalars(
                            &match &current {
                                PlaceStorage::Slot { slot, offset } => PlaceStorage::Slot {
                                    slot: *slot,
                                    offset: *offset,
                                },
                                PlaceStorage::Address { base, offset } => PlaceStorage::Address {
                                    base: *base,
                                    offset: *offset,
                                },
                                PlaceStorage::Values(values) => {
                                    PlaceStorage::Values(values.clone())
                                }
                            },
                            &current_type,
                            layout::type_alignment(&current_type),
                        )[0]
                        .into_pointer_value(),
                        _ => unreachable!(
                            "mir body guarantees Index projections only apply to Array/SizedArray/Slice/Str"
                        ),
                    };

                    let mut index = self.process_expr(index_expr)[0].into_int_value();
                    let ptr_int: BasicTypeEnum = if self.pointer_bytes() == 8 {
                        self.context.i64_type().into()
                    } else {
                        self.context.i32_type().into()
                    };
                    if index.get_type() != ptr_int.into_int_type() {
                        index = self
                            .builder
                            .build_int_z_extend(index, ptr_int.into_int_type(), "index")
                            .expect("zext always succeeds");
                    }
                    let scaled = self
                        .builder
                        .build_int_mul(
                            index,
                            ptr_int
                                .into_int_type()
                                .const_int(element_size as u64, false),
                            "offset",
                        )
                        .expect("mul always succeeds");
                    base = self.gep(base, scaled);

                    current = PlaceStorage::Address { base, offset: 0 };
                    current_type = item_type.clone();
                }

                MirProjection::EnumTag { r#type } => {
                    let ResolvedType::Enum { cell, .. } = &current_type else {
                        unreachable!(
                            "mir body guarantees EnumTag projections are only built against an enum type"
                        );
                    };
                    let tag_leaves = leaf::llvm_leaves(
                        self.context,
                        &cell.borrow().tag_type,
                        self.pointer_bytes(),
                    )
                    .len();
                    current = match current {
                        PlaceStorage::Values(values) => {
                            PlaceStorage::Values(values[..tag_leaves].to_vec())
                        }
                        memory_backed => memory_backed,
                    };
                    current_type = r#type.clone();
                }

                MirProjection::EnumHeader { index, r#type, .. } => {
                    let ResolvedType::Enum { cell, .. } = &current_type else {
                        unreachable!(
                            "mir body guarantees EnumHeader projections are only built against an enum type"
                        );
                    };
                    let cell = cell.clone();
                    let enum_type = cell.borrow();
                    current = match current {
                        PlaceStorage::Values(values) => {
                            let start =
                                layout::enum_prefix_layout(&enum_type, self.pointer_bytes())
                                    .leaf_starts[1 + *index];
                            let len = leaf::llvm_leaves(
                                self.context,
                                &enum_type.header[*index].r#type,
                                self.pointer_bytes(),
                            )
                            .len();
                            PlaceStorage::Values(values[start..start + len].to_vec())
                        }
                        PlaceStorage::Slot { slot, offset } => PlaceStorage::Slot {
                            slot,
                            offset: offset
                                + layout::enum_header_offset(
                                    &enum_type,
                                    *index,
                                    self.pointer_bytes(),
                                ),
                        },
                        PlaceStorage::Address { base, offset } => PlaceStorage::Address {
                            base,
                            offset: offset
                                + layout::enum_header_offset(
                                    &enum_type,
                                    *index,
                                    self.pointer_bytes(),
                                ),
                        },
                    };
                    current_type = r#type.clone();
                }

                MirProjection::EnumDynamicField { index, r#type, .. } => {
                    let ResolvedType::Enum { cell, .. } = &current_type else {
                        unreachable!(
                            "mir body guarantees EnumDynamicField projections are only built against an enum type"
                        );
                    };
                    let cell = cell.clone();
                    let enum_type = cell.borrow();
                    current = match current {
                        PlaceStorage::Values(values) => {
                            let start =
                                layout::enum_prefix_layout(&enum_type, self.pointer_bytes())
                                    .leaf_starts[1 + enum_type.header.len() + *index];
                            let len = leaf::llvm_leaves(
                                self.context,
                                &enum_type.dynamic_fields[*index].r#type,
                                self.pointer_bytes(),
                            )
                            .len();
                            PlaceStorage::Values(values[start..start + len].to_vec())
                        }
                        PlaceStorage::Slot { slot, offset } => PlaceStorage::Slot {
                            slot,
                            offset: offset
                                + layout::enum_dynamic_field_offset(
                                    &enum_type,
                                    *index,
                                    self.pointer_bytes(),
                                ),
                        },
                        PlaceStorage::Address { base, offset } => PlaceStorage::Address {
                            base,
                            offset: offset
                                + layout::enum_dynamic_field_offset(
                                    &enum_type,
                                    *index,
                                    self.pointer_bytes(),
                                ),
                        },
                    };
                    current_type = r#type.clone();
                }

                MirProjection::EnumBody {
                    variant_index,
                    field_index,
                    r#type,
                    ..
                } => {
                    let ResolvedType::Enum { cell, .. } = &current_type else {
                        unreachable!(
                            "mir body guarantees EnumBody projections are only built against an enum type"
                        );
                    };
                    let cell = cell.clone();
                    let enum_type = cell.borrow();
                    current = match current {
                        PlaceStorage::Values(values) => {
                            let start =
                                layout::enum_prefix_layout(&enum_type, self.pointer_bytes())
                                    .leaf_starts[1
                                    + enum_type.header.len()
                                    + enum_type.dynamic_fields.len()
                                    + *field_index];
                            let len = leaf::llvm_leaves(
                                self.context,
                                &enum_type.variants[*variant_index].fields[*field_index].r#type,
                                self.pointer_bytes(),
                            )
                            .len();
                            PlaceStorage::Values(values[start..start + len].to_vec())
                        }
                        PlaceStorage::Slot { slot, offset } => PlaceStorage::Slot {
                            slot,
                            offset: offset
                                + layout::enum_body_field_offset(
                                    &enum_type,
                                    *variant_index,
                                    *field_index,
                                    self.pointer_bytes(),
                                ),
                        },
                        PlaceStorage::Address { base, offset } => PlaceStorage::Address {
                            base,
                            offset: offset
                                + layout::enum_body_field_offset(
                                    &enum_type,
                                    *variant_index,
                                    *field_index,
                                    self.pointer_bytes(),
                                ),
                        },
                    };
                    current_type = r#type.clone();
                }

                // Slice indexing uses the data-pointer leaf; the length leaf is not part of the address calculation.
                MirProjection::SliceLength => {
                    let ptr_size = self.pointer_bytes();
                    current = match current {
                        PlaceStorage::Values(values) => PlaceStorage::Values(vec![values[1]]),
                        PlaceStorage::Slot { slot, offset } => PlaceStorage::Slot {
                            slot,
                            offset: offset + ptr_size,
                        },
                        PlaceStorage::Address { base, offset } => PlaceStorage::Address {
                            base,
                            offset: offset + ptr_size,
                        },
                    };
                    current_type = ResolvedType::I32;
                }

                // Dynamic-spec places use the data-pointer leaf; the vtable leaf is metadata.
                MirProjection::SpecObjectPtr { mutable } => {
                    current = match current {
                        PlaceStorage::Values(values) => PlaceStorage::Values(vec![values[0]]),
                        other => other,
                    };
                    current_type = ResolvedType::Pointer {
                        pointee: Box::new(ResolvedType::U8),
                        mutable: *mutable,
                    };
                }

                MirProjection::SpecObjectVtable => {
                    let ptr_size = self.pointer_bytes();
                    current = match current {
                        PlaceStorage::Values(values) => PlaceStorage::Values(vec![values[1]]),
                        PlaceStorage::Slot { slot, offset } => PlaceStorage::Slot {
                            slot,
                            offset: offset + ptr_size,
                        },
                        PlaceStorage::Address { base, offset } => PlaceStorage::Address {
                            base,
                            offset: offset + ptr_size,
                        },
                    };
                    current_type = ResolvedType::Pointer {
                        pointee: Box::new(ResolvedType::U8),
                        mutable: false,
                    };
                }
            }
        }

        (current, current_type, place.align)
    }

    fn offset_align(base_align: u32, byte_offset: u32) -> u32 {
        let base_align = base_align.max(1);
        if byte_offset == 0 {
            base_align
        } else {
            base_align.min(1u32 << byte_offset.trailing_zeros())
        }
    }

    pub(super) fn load_scalars(
        &mut self,
        storage: &PlaceStorage<'ctx>,
        r#type: &ResolvedType,
        align: u32,
    ) -> Vec<BasicValueEnum<'ctx>> {
        if let PlaceStorage::Values(values) = storage {
            return values.clone();
        }

        let mut result = Vec::new();
        let mut rel_offset = 0u32;
        for raw_leaf in omega_analyzer::layout::leaves_of(r#type, self.pointer_bytes()) {
            let llvm_ty = leaf::llvm_type(self.context, raw_leaf, self.pointer_bytes());
            let value = match storage {
                PlaceStorage::Slot { slot, offset } => {
                    let at = *offset + rel_offset;
                    self.aligned_load(llvm_ty, *slot, at, Self::offset_align(align, at))
                }
                PlaceStorage::Address { base, offset } => {
                    let at = *offset + rel_offset;
                    self.aligned_load(llvm_ty, *base, at, Self::offset_align(align, at))
                }
                PlaceStorage::Values(_) => unreachable!("handled above"),
            };
            result.push(value);
            rel_offset += raw_leaf.bytes(self.pointer_bytes());
        }
        result
    }

    fn aligned_load(
        &self,
        ty: BasicTypeEnum<'ctx>,
        base: PointerValue<'ctx>,
        offset: u32,
        align: u32,
    ) -> BasicValueEnum<'ctx> {
        let ptr = self.byte_gep(base, offset);
        let value = self
            .builder
            .build_load(ty, ptr, "")
            .expect("load always succeeds");
        if let Some(inst) = value.as_instruction_value() {
            let _ = inst.set_alignment(align);
        }
        value
    }

    pub(super) fn store_scalars(
        &mut self,
        base: &PointerValue<'ctx>,
        base_offset: u32,
        values: &[BasicValueEnum<'ctx>],
        align: u32,
    ) {
        let mut rel_offset = 0u32;
        for value in values {
            let leaf_bytes = leaf::value_byte_width(value.get_type(), self.pointer_bytes());
            let at = base_offset + rel_offset;
            let ptr = self.byte_gep(*base, at);
            let store = self
                .builder
                .build_store(ptr, *value)
                .expect("store always succeeds");
            let _ = store.set_alignment(Self::offset_align(align, at));
            rel_offset += leaf_bytes;
        }
    }

    pub(super) fn place_storage_address(
        &mut self,
        storage: &PlaceStorage<'ctx>,
    ) -> PointerValue<'ctx> {
        match storage {
            PlaceStorage::Values(values) => {
                let size: u32 = values
                    .iter()
                    .map(|v| leaf::value_byte_width(v.get_type(), self.pointer_bytes()))
                    .sum();
                let slot = self.entry_alloca(size, 16, "param_addr");
                self.store_scalars(&slot, 0, values, 1);
                slot
            }
            PlaceStorage::Slot { slot, offset } => self.byte_gep(*slot, *offset),
            PlaceStorage::Address { base, offset } => self.byte_gep(*base, *offset),
        }
    }

    fn byte_gep(&self, base: PointerValue<'ctx>, offset: u32) -> PointerValue<'ctx> {
        let int_ty: BasicTypeEnum = if self.pointer_bytes() == 8 {
            self.context.i64_type().into()
        } else {
            self.context.i32_type().into()
        };
        self.gep(base, int_ty.into_int_type().const_int(offset as u64, false))
    }

    pub(super) fn gep(
        &self,
        base: PointerValue<'ctx>,
        offset_value: inkwell::values::IntValue<'ctx>,
    ) -> PointerValue<'ctx> {
        unsafe {
            self.builder
                .build_in_bounds_gep(self.context.i8_type(), base, &[offset_value], "gep")
        }
        .expect("gep always succeeds")
    }
}
