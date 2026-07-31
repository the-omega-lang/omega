//! Resolving a [`MirPlace`] to its underlying storage -- the one general
//! mechanism behind reading, writing, and taking the address of a place,
//! regardless of how many derefs/field accesses/indices got it there.

use super::Codegen;
use super::leaf::IntoCraneliftLeaves;
use omega_analyzer::layout;
use cranelift::codegen::ir::StackSlot;
use cranelift::prelude::{FunctionBuilder, InstBuilder, MemFlags, StackSlotData, StackSlotKind, Value};
use cranelift_module::Module;
use omega_analyzer::resolved_type::ResolvedType;
use omega_mir::{MirPlace, MirPlaceRoot, MirProjection};

/// Where a resolved place's underlying storage lives, for both the read
/// (producing values) and write (storing values) case:
pub(super) enum PlaceStorage {
    /// Already-materialized SSA values (a parameter local that hasn't been
    /// dereferenced through) -- readable, but has no address: there is no
    /// memory location backing a bare SSA value.
    Values(Vec<Value>),
    /// A byte offset into one compile-time-known stack slot (a non-
    /// parameter local, before any `Deref`).
    Slot { slot: StackSlot, offset: u32 },
    /// A byte offset from a runtime pointer value -- the state from the
    /// first `Deref` projection onward (explicit `*`, or a seamless
    /// pointer-to-struct field access), since the pointee isn't known
    /// until runtime.
    Address { base: Value, offset: u32 },
}

impl Codegen {
    /// Walks a place's root and projections once, tracking where its
    /// storage currently lives -- switching from `Slot`/`Values` to
    /// `Address` the moment a `Deref` (explicit, or an array `Index`'s
    /// implicit pointer arithmetic) happens, since the pointee isn't known
    /// until runtime.
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
                    let slot = match self.stack_slots[id.0 as usize] {
                        Some(slot) => slot,
                        None => {
                            let shift = layout::stack_align_shift(layout::type_alignment(r#type));
                            let size = layout::total_bytes(r#type, self.pointer_bytes());
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
            // A global's own runtime address, exactly like a vtable's or an
            // interned string's (`declare_data_in_func` + `global_value`) --
            // then an ordinary `Address` storage from there, offset 0, so
            // every projection below walks it exactly like a dereferenced
            // pointer's storage already does.
            MirPlaceRoot::Global { id, r#type } => {
                let data_id = *self.globals.get(id).unwrap_or_else(|| {
                    panic!("mir body guarantees {id:?} was declared as a global before this use")
                });
                let global_value = self.module.declare_data_in_func(data_id, builder.func);
                let base = builder.ins().global_value(self.pointer_type(), global_value);
                (PlaceStorage::Address { base, offset: 0 }, r#type.clone())
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

                // Every field lives at offset 0 (see
                // `MirProjection::UnionField`'s doc comment) -- the only
                // real work here is spilling an SSA-value-backed union to
                // memory first (mirrors `EnumBody`'s identical spill, for the
                // identical reason: no leaf slice can reinterpret one field's
                // real shape out of the union's own opaque payload chunks),
                // then letting `current_type` advance to the field's type.
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
                    // The element size comes from `item_type` (the resolved
                    // element type analysis already picked out), not from
                    // flattening `current_type` itself -- the container's own
                    // leaf flattening (a single thin pointer for `Array`, or
                    // N*item leaves for `SizedArray`) has nothing to do with
                    // one element's size.
                    let element_ir_size = layout::total_bytes(item_type, self.pointer_bytes());

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
                            // Positional, by leaf-list start index -- shares
                            // `enum_header_offset`'s exact layout (see
                            // `enum_prefix_layout`), so an interior gap
                            // before this field (from an earlier field's own
                            // alignment demand) is accounted for here too.
                            let start = layout::enum_prefix_layout(&enum_type, self.pointer_bytes()).leaf_starts
                                [1 + *index];
                            let len = enum_type.header[*index].1.cranelift_leaves(self).len();
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
                            let start = layout::enum_prefix_layout(&enum_type, self.pointer_bytes()).leaf_starts
                                [1 + enum_type.header.len() + *index];
                            let len = enum_type.dynamic_fields[*index].1.cranelift_leaves(self).len();
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
                    // A body field lives inside the opaque payload chunks,
                    // which no leaf slice can address -- an SSA-value-backed
                    // enum (a parameter) is spilled to an anonymous slot
                    // first, exactly like a temporary place root, so the
                    // field is an ordinary byte offset from there.
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
                    // A slice is flattened as [data pointer, i32 length] --
                    // `.length` is just the second leaf, at a byte offset of
                    // one pointer's width past the start of the slice's own
                    // storage.
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
    pub(super) fn place_storage_address(&mut self, builder: &mut FunctionBuilder, storage: &PlaceStorage) -> Value {
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

    /// Writes `values` (one per scalar leaf, in leaf order) into `storage`.
    pub(super) fn store_scalars(&mut self, builder: &mut FunctionBuilder, storage: &PlaceStorage, values: &[Value]) {
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

    pub(super) fn get_place_value(&mut self, place: &MirPlace, builder: &mut FunctionBuilder) -> Vec<Value> {
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
}
