//! Resolving a `MirPlace` to concrete storage, and loading/storing scalar
//! leaves through it -- the LLVM counterpart of `cranelift/place.rs`, with
//! one deliberate difference: **every load and store carries the
//! `MirPlace::align` alignment explicitly** (see that field's doc comment
//! for why LLVM's natural-alignment default would be a miscompile on
//! Omega's packed layouts).

use super::leaf;
use super::Codegen;
use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicValue, BasicValueEnum, PointerValue};
use omega_analyzer::layout;
use omega_analyzer::resolved_type::ResolvedType;
use omega_mir::{MirPlace, MirPlaceRoot, MirProjection};

/// Where a resolved place's value physically lives -- the exact shape
/// `cranelift/place.rs`'s `PlaceStorage` takes, translated to LLVM:
/// `Values` (SSA leaves), `Slot` (a byte offset into the shared frame
/// alloca), or `Address` (a byte-offset base pointer).
#[derive(Clone)]
pub(super) enum PlaceStorage<'ctx> {
    Values(Vec<BasicValueEnum<'ctx>>),
    Slot { slot: PointerValue<'ctx>, offset: u32 },
    Address { base: PointerValue<'ctx>, offset: u32 },
}

impl<'ctx> Codegen<'ctx> {
    /// `resolve_place_storage`'s LLVM counterpart -- the same projection
    /// walk, the same `current`/`current_type` bookkeeping, returning the
    /// final storage, type, and the access alignment the place carries.
    pub(super) fn resolve_place_storage(
        &mut self,
        place: &MirPlace,
    ) -> (PlaceStorage<'ctx>, ResolvedType, u32) {
        let (mut current, mut current_type) = match &place.root {
            MirPlaceRoot::Local { id, r#type } => {
                let current = if (id.0 as usize) < self.arg_count {
                    PlaceStorage::Values(self.local_args[id.0 as usize].clone())
                } else {
                    let slot = self.frame_slot.expect(
                        "define_function_def always sets this before any block runs (a zero-size \
                         frame still means a local's address is the frame's own base)",
                    );
                    PlaceStorage::Slot { slot, offset: self.local_offsets[id.0 as usize] }
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
            // A temporary as the root of a projection chain -- materialized
            // into an anonymous stack slot, exactly like Cranelift's
            // `MirPlaceRoot::Expr` arm (spill `Values` to a fresh alloca).
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

                MirProjection::UnionField { r#type, .. } => {
                    if let PlaceStorage::Values(values) = &current {
                        let shift = layout::stack_align_shift(layout::type_alignment(&current_type));
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
                            PlaceStorage::Slot { slot, offset } => PlaceStorage::Slot { slot: *slot, offset: *offset },
                            PlaceStorage::Address { base, offset } => PlaceStorage::Address { base: *base, offset: *offset },
                            PlaceStorage::Values(values) => PlaceStorage::Values(values.clone()),
                        },
                        &current_type,
                        layout::type_alignment(&current_type),
                    )[0]
                        .into_pointer_value();
                    current = PlaceStorage::Address { base: ptr_value, offset: 0 };
                    current_type = r#type.clone();
                }

                MirProjection::Index { index_expr, item_type } => {
                    let element_size = layout::total_bytes(item_type, self.pointer_bytes());

                    let mut base = match &current_type {
                        ResolvedType::SizedArray(_, _) => self.place_storage_address(
                            &match &current {
                                PlaceStorage::Slot { slot, offset } => PlaceStorage::Slot { slot: *slot, offset: *offset },
                                PlaceStorage::Address { base, offset } => PlaceStorage::Address { base: *base, offset: *offset },
                                PlaceStorage::Values(values) => PlaceStorage::Values(values.clone()),
                            },
                        ),
                        ResolvedType::Array(_, _) | ResolvedType::Slice { .. } | ResolvedType::Str { .. } => self
                            .load_scalars(
                                &match &current {
                                    PlaceStorage::Slot { slot, offset } => PlaceStorage::Slot { slot: *slot, offset: *offset },
                                    PlaceStorage::Address { base, offset } => PlaceStorage::Address { base: *base, offset: *offset },
                                    PlaceStorage::Values(values) => PlaceStorage::Values(values.clone()),
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
                        unreachable!("mir body guarantees EnumTag projections are only built against an enum type");
                    };
                    let tag_leaves = leaf::llvm_leaves(self.context, &cell.borrow().tag_type, self.pointer_bytes()).len();
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
                            let start = layout::enum_prefix_layout(&enum_type, self.pointer_bytes()).leaf_starts
                                [1 + *index];
                            let len = leaf::llvm_leaves(self.context, &enum_type.header[*index].1, self.pointer_bytes()).len();
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
                            let len = leaf::llvm_leaves(self.context, &enum_type.dynamic_fields[*index].1, self.pointer_bytes()).len();
                            PlaceStorage::Values(values[start..start + len].to_vec())
                        }
                        PlaceStorage::Slot { slot, offset } => PlaceStorage::Slot {
                            slot,
                            offset: offset + layout::enum_dynamic_field_offset(&enum_type, *index, self.pointer_bytes()),
                        },
                        PlaceStorage::Address { base, offset } => PlaceStorage::Address {
                            base,
                            offset: offset + layout::enum_dynamic_field_offset(&enum_type, *index, self.pointer_bytes()),
                        },
                    };
                    current_type = r#type.clone();
                }

                MirProjection::EnumBody { variant_index, field_index, r#type, .. } => {
                    let ResolvedType::Enum { cell, .. } = &current_type else {
                        unreachable!("mir body guarantees EnumBody projections are only built against an enum type");
                    };
                    let cell = cell.clone();
                    let enum_type = cell.borrow();
                    current = match current {
                        PlaceStorage::Values(values) => {
                            let start = layout::enum_prefix_layout(&enum_type, self.pointer_bytes()).leaf_starts
                                [1 + enum_type.header.len() + enum_type.dynamic_fields.len() + *field_index];
                            let len = leaf::llvm_leaves(
                                self.context,
                                &enum_type.variants[*variant_index].fields[*field_index].1,
                                self.pointer_bytes(),
                            )
                            .len();
                            PlaceStorage::Values(values[start..start + len].to_vec())
                        }
                        PlaceStorage::Slot { slot, offset } => PlaceStorage::Slot {
                            slot,
                            offset: offset + layout::enum_body_field_offset(&enum_type, *variant_index, *field_index, self.pointer_bytes()),
                        },
                        PlaceStorage::Address { base, offset } => PlaceStorage::Address {
                            base,
                            offset: offset + layout::enum_body_field_offset(&enum_type, *variant_index, *field_index, self.pointer_bytes()),
                        },
                    };
                    current_type = r#type.clone();
                }

                // A slice is flattened as [data pointer, i32 length], so
                // `.length` is the second leaf -- element 1, one pointer's
                // width past the data pointer. `I32`, not `USize`: the
                // length leaf is genuinely 32-bit (`layout::leaves_of`).
                MirProjection::SliceLength => {
                    let ptr_size = self.pointer_bytes();
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

                // A spec object is flattened as [data pointer, vtable
                // pointer] -- `.ptr` is the first leaf at no offset,
                // `.vtable` the second, exactly like `SliceLength` above.
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
                        PlaceStorage::Slot { slot, offset } => {
                            PlaceStorage::Slot { slot, offset: offset + ptr_size }
                        }
                        PlaceStorage::Address { base, offset } => {
                            PlaceStorage::Address { base, offset: offset + ptr_size }
                        }
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

    /// The alignment an access `byte_offset` bytes into a `base_align`-
    /// aligned base actually has. `MirPlace::align` claims only the
    /// place's *base* address, so a byte-offset access (a leaf within a
    /// flattened aggregate, a field within a slot) must weaken that claim
    /// by whatever the offset itself destroys, or an over-aligned `align`
    /// on the resulting load/store is UB LLVM's optimizer can act on.
    /// Only ever lowers the claim -- see `docs/14-known-issues.md`'s
    /// `@layout(align)` entry for the propagation gap this does not cover.
    fn offset_align(base_align: u32, byte_offset: u32) -> u32 {
        let base_align = base_align.max(1);
        if byte_offset == 0 {
            base_align
        } else {
            base_align.min(1u32 << byte_offset.trailing_zeros())
        }
    }

    /// Reads a whole place's leaves back from `storage` -- leaf by leaf,
    /// with the access's own alignment set explicitly on every load (see
    /// the module doc comment and `offset_align`).
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

    /// A load from a byte-offset address, with an explicit alignment --
    /// LLVM's default is *natural* alignment, and Omega's packed layouts
    /// are genuinely unaligned (see `MirPlace::align`'s doc comment).
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

    /// Writes `values` (one per scalar leaf, in leaf order) into `storage`
    /// -- leaf by leaf, with the access's own alignment set explicitly on
    /// every store (see `offset_align`).
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

    /// The address of a place's storage -- spilling a parameter's SSA
    /// values into a fresh alloca on demand (the same lazy materialization
    /// `cranelift/place.rs`'s `place_storage_address` does), so an
    /// explicit `&param` or an implicit auto-ref can take its address like
    /// any other in-memory place's.
    pub(super) fn place_storage_address(&mut self, storage: &PlaceStorage<'ctx>) -> PointerValue<'ctx> {
        match storage {
            PlaceStorage::Values(values) => {
                let size: u32 = values.iter().map(|v| leaf::value_byte_width(v.get_type(), self.pointer_bytes())).sum();
                let slot = self.entry_alloca(size, 16, "param_addr");
                self.store_scalars(&slot, 0, values, 1);
                slot
            }
            PlaceStorage::Slot { slot, offset } => self.byte_gep(*slot, *offset),
            PlaceStorage::Address { base, offset } => self.byte_gep(*base, *offset),
        }
    }

    /// `base + offset`, in universal `i8*` terms.
    fn byte_gep(&self, base: PointerValue<'ctx>, offset: u32) -> PointerValue<'ctx> {
        let int_ty: BasicTypeEnum = if self.pointer_bytes() == 8 {
            self.context.i64_type().into()
        } else {
            self.context.i32_type().into()
        };
        self.gep(base, int_ty.into_int_type().const_int(offset as u64, false))
    }

    /// The one safe wrapper around LLVM's (unsafe-flagged) in-bounds GEP:
    /// `base + offset_value`, in universal `i8*` terms.
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
