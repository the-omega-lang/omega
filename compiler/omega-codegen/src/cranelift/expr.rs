//! Evaluating one [`MirExprNode`] into its scalar leaves -- the bulk of
//! what this backend actually does, since every control-flow shape was
//! already flattened away by `omega-mir` (see `docs/16-mir-and-
//! codegen.md`); this is purely expression evaluation.

use super::Codegen;
use super::leaf::IntoCraneliftLeaves;
use super::place::PlaceStorage;
use crate::layout;
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

impl Codegen {
    /// A byte-run constant's two-leaf `[pointer, length]` form -- the
    /// shape both `ResolvedType::Slice` and `ResolvedType::Str`'s leaf
    /// flattening expect (identical for both) -- deduplicated per module
    /// (`bytes`) and per function (`local_bytes`, which only caches the
    /// pointer; the length is a cheap `iconst` recomputed each call, same
    /// as any other compile-time-constant length). Shared by string
    /// literal expressions, byte-string literal expressions, and enum
    /// header/dynamic-field constants -- the caller alone decides whether
    /// the surrounding value is typed `*str` or `*[u8]`.
    fn emit_bytes(&mut self, builder: &mut FunctionBuilder, s: String) -> Vec<Value> {
        let len = builder.ins().iconst(types::I32, s.len() as i64);

        if let Some(local_value) = self.local_bytes.get(&s) {
            return vec![*local_value, len];
        }

        let ptr_type = self.pointer_type();
        let data_id = if let Some(id) = self.bytes.get(&s) { *id } else { self.get_or_declare_global_bytes(s.clone()) };

        let global_value = self.module.declare_data_in_func(data_id, builder.func);
        let ptr = builder.ins().global_value(ptr_type, global_value);

        self.local_bytes.insert(s, ptr);

        vec![ptr, len]
    }

    /// An anonymous data object's symbol -- a pure function of its own
    /// bytes, not an arbitrary per-process counter: two identical
    /// constants, in the same compilation or two separate ones, always
    /// name themselves identically, the same "stable, type/content-derived
    /// name" property `omega_mangle` gives real functions/methods -- see
    /// its own design notes for why that matters. Rapidhash V3
    /// (`rapidhash::v3::rapidhash_v3`, avalanche-enabled -- its default,
    /// matching the reference C++ implementation, chosen over the crate's
    /// own `fast` preset since that one deliberately trades away mixing
    /// quality for hashmap-bucket-selection speed, a different need than a
    /// standalone, collision-averse identifier) is a deliberately
    /// non-cryptographic choice: nothing here is adversarial (the input is
    /// always the compiler's own already-resolved constant data), so all
    /// that's needed is a fast hash with a low *accidental* collision rate
    /// at realistic program sizes, not preimage/collision resistance
    /// against a deliberate attacker.
    fn data_symbol(bytes: &[u8]) -> String {
        format!("_omgdata_{:016x}", rapidhash::v3::rapidhash_v3(bytes))
    }

    /// Declares (and defines) `s`'s bytes as an anonymous module-level data
    /// object, verbatim -- no null terminator, shared by `"..."` (`*str`)
    /// and `b"..."` (`*[u8]`) literals alike (see `bytes`'s own doc
    /// comment for why one function/map correctly serves both now).
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

    /// Emits one `ConstValue` (an enum tag/header constant, or a
    /// `MirExpr::ConstSlice`) as its leaves, in leaf order -- every
    /// variant but `Slice`/`Array` is exactly one IR leaf; `Slice` is the
    /// two-leaf `[ptr, len]` fat pointer every other `ResolvedType::Slice`
    /// value already is (see `emit_const_slice`); `Array` is every
    /// element's own leaves concatenated in order, with no indirection at
    /// all -- the same packed, no-padding layout a `SizedArray`'s own leaf
    /// flattening already uses.
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
        }
    }

    /// A compile-time slice's `[ptr, len]` leaves -- unlike `emit_bytes`,
    /// deliberately *not* deduplicated across call sites: `ConstValue`
    /// isn't cheaply hashable (it nests, and `NumberValue::Float` has no
    /// total order), and each occurrence is a one-shot codegen site (one
    /// enum construction, or one `&[...]` expression) rather than
    /// something plausibly repeated many times per function the way
    /// string literals are.
    fn emit_const_slice(&mut self, builder: &mut FunctionBuilder, elements: &[ConstValue], item_type: &ResolvedType) -> Vec<Value> {
        let ptr_type = self.pointer_type();
        let len = builder.ins().iconst(types::I32, elements.len() as i64);
        let data_id = self.build_const_slice_data(elements, item_type);
        let global_value = self.module.declare_data_in_func(data_id, builder.func);
        let ptr = builder.ins().global_value(ptr_type, global_value);
        vec![ptr, len]
    }

    /// Builds one anonymous, module-level data object holding `elements`
    /// laid out at consecutive `total_bytes(item_type)`-sized slots -- the
    /// same packed layout a `SizedArray`'s own leaf flattening already
    /// uses, so the result is byte-for-byte what an ordinary runtime slice
    /// over this data would expect. `&[]` never reaches here --
    /// `EmptyArrayLiteral` already rejects it at analysis, same as a bare
    /// `[]`.
    fn build_const_slice_data(&mut self, elements: &[ConstValue], item_type: &ResolvedType) -> DataId {
        let stride = layout::total_bytes(item_type, self.pointer_bytes());
        let mut bytes = vec![0u8; stride as usize * elements.len()];
        let mut desc = DataDescription::new();
        for (i, element) in elements.iter().enumerate() {
            self.write_const_element(&mut desc, &mut bytes, i as u32 * stride, element, item_type);
        }
        desc.define(bytes.into_boxed_slice());

        let mut hash_input = Vec::new();
        for element in elements {
            self.hash_const_element(&mut hash_input, element, item_type);
        }
        let sym = Self::data_symbol(&hash_input);
        let id = self.module.declare_data(&sym, cranelift_module::Linkage::Preemptible, false, false).unwrap();
        self.module.define_data(id, &desc).unwrap();
        id
    }

    /// Appends `value`'s canonical, unambiguous content bytes to `out`,
    /// purely for `data_symbol`'s naming purposes -- deliberately *not*
    /// the same bytes `write_const_element` writes into the real data
    /// object. `write_const_element` leaves a pointer-shaped element
    /// (`Str`, nested `Slice`) as a zero placeholder in the physical
    /// buffer -- the actual target only exists as a `write_data_addr`
    /// relocation recorded in `desc`, invisible to a hash over raw bytes
    /// alone. Hashing the physical buffer directly would therefore let
    /// two constant slices that point at *different* strings collide on
    /// one symbol name whenever their non-pointer bytes happen to
    /// coincide (e.g. `&["a"]` and `&["b"]`, both a single same-length
    /// string) -- harmless under today's `Local` linkage, but a real
    /// silent miscompile risk if these ever move to weak/COMDAT linkage
    /// (two genuinely different constants folded into one because the
    /// linker trusted a colliding name). So this walks the *logical*
    /// `ConstValue` tree instead, writing a string's real bytes (length-
    /// prefixed, since it's the only variable-length leaf here) rather
    /// than a placeholder. Every other leaf is fixed-width (given
    /// `r#type`, shared across one call) or already length-prefixed
    /// (`Slice`'s element count), so the whole traversal is
    /// self-delimiting with no separators needed.
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
        }
    }

    /// Writes one element's leaves into `bytes`/`desc` at `offset`. A
    /// scalar (`Number`/`Bool`/`Char`) is written as literal little-endian
    /// bytes -- its address never depends on the linker. A pointer-shaped
    /// element (`Str`, or a nested `Slice`) can't have its address known
    /// until link/load time, so it's written as a
    /// `DataDescription::write_data_addr` relocation into its own
    /// (recursively built, for `Slice`) data object instead -- the same
    /// "embed a pointer to other static data" mechanism object file
    /// formats already support for e.g. initialized pointer tables. A
    /// nested `Slice` element's trailing length leaf has no such address
    /// dependency, so it's still a literal byte write.
    fn write_const_element(
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

                // `*str` (unlike the old, always-null-terminated `*u8` this
                // used to be) is a fat pointer -- the length leaf needs
                // writing too, exactly like `ConstValue::Slice` below.
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
            // No indirection at all (unlike `Slice`/`Str` above) -- every
            // element is written inline, back to back, into this same
            // buffer, exactly like `emit_const_value`'s `Array` case does
            // for the function-local (non-static-data) form.
            ConstValue::Array(elements) => {
                let ResolvedType::SizedArray(item, _) = r#type else {
                    unreachable!("mir body guarantees a nested Array constant's own type is SizedArray");
                };
                let stride = layout::total_bytes(item, self.pointer_bytes());
                for (i, element) in elements.iter().enumerate() {
                    self.write_const_element(desc, bytes, offset + i as u32 * stride, element, item);
                }
            }
        }
    }

    /// C's variadic calling convention requires the caller to promote each
    /// *variadic* argument (never a fixed/named one, whose width is fixed by
    /// the callee's prototype) before passing it: any integer narrower than
    /// `int` is sign/zero-extended to 32 bits, and `float` is promoted to
    /// `double` -- otherwise a callee like `printf` (which reads variadic
    /// arguments according to those default-promoted widths, per its format
    /// string) would read garbage. Only applies to `arg_type`s that flatten
    /// to exactly one IR leaf (every numeric primitive does); called
    /// unconditionally on every variadic argument, so anything else (a
    /// pointer, already the right width) just passes through unchanged.
    fn promote_variadic_arg(&mut self, builder: &mut FunctionBuilder, value: Value, arg_type: &ResolvedType) -> Value {
        match arg_type.numeric_kind() {
            Some(NumericKind::Float(width)) if width < 64 => builder.ins().fpromote(types::F64, value),
            Some(NumericKind::Signed(width)) if width < 32 => builder.ins().sextend(types::I32, value),
            Some(NumericKind::Unsigned(width)) if width < 32 => builder.ins().uextend(types::I32, value),
            // `Bool` isn't `numeric_kind`-classified (see its doc comment),
            // but it's still an 8-bit integer that needs the same promotion.
            None if *arg_type == ResolvedType::Bool => builder.ins().uextend(types::I32, value),
            _ => value,
        }
    }

    /// If `fn_type`'s return value needs the hidden struct-return
    /// convention (see `needs_sret`), allocates the scratch slot for it
    /// and prepends its address to `ir_args` -- shared by an ordinary call
    /// and a dynamic (vtable) one, which otherwise duplicated this
    /// verbatim.
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

    /// Builds the (possibly variadic-patched) signature for an indirect
    /// call and emits it -- shared by an ordinary call and a dynamic
    /// (vtable) one (the latter's `fn_type` is never actually variadic,
    /// but there's nothing wrong with asking; the patch is a no-op unless
    /// `ir_args` genuinely has more entries than the declared params).
    fn emit_call_indirect(
        &mut self,
        builder: &mut FunctionBuilder,
        fnaddr: Value,
        fn_type: &ResolvedFunctionType,
        ir_args: &[Value],
    ) -> Inst {
        // Cranelift does not support variadic functions directly. To
        // bypass that, the call convention is already `SystemV` (see
        // `make_function_sig`), and any *extra* (variadic) arguments
        // beyond the fixed, declared params just get their own
        // already-materialized Cranelift type appended to the signature
        // used for this one call site.
        let mut sig = self.make_function_sig(fn_type.clone());
        if fn_type.is_variadic && ir_args.len() > sig.params.len() {
            for arg in &ir_args[sig.params.len()..] {
                sig.params.push(AbiParam::new(builder.func.dfg.value_type(*arg)));
            }
        }
        let sigref = builder.import_signature(sig);
        builder.ins().call_indirect(sigref, fnaddr, ir_args)
    }

    /// Reads a call's return value back -- through `sret_slot`'s memory if
    /// one was allocated (see `maybe_sret_arg`), or straight from the
    /// instruction's own results otherwise. Shared by an ordinary call and
    /// a dynamic (vtable) one.
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
            MirExpr::ConstSlice(value) => self.emit_const_value(builder, &value, &node.r#type),

            MirExpr::FunctionCall(MirFunctionCall { callee, fn_type, args }) => {
                // The mir guarantees the callee resolves to exactly one
                // Function-typed value -- there is no way to construct a
                // Function-typed expression other than a function place
                // root, which always yields a single address.
                let fnaddr = self.process_expr(builder, *callee)[0];

                let fixed_count = fn_type.params.len();
                let mut ir_args = vec![];
                for (i, arg) in args.into_iter().enumerate() {
                    let arg_type = arg.r#type.clone();
                    let mut value = self.process_expr(builder, arg);
                    // Only the variadic tail needs default-argument
                    // promotion; a fixed/named parameter's width is already
                    // pinned by the callee's declared signature.
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

            // `*Concrete` -> `spec *Spec`: builds the fat pointer's two
            // leaves -- `base`'s own value unchanged (the data pointer)
            // plus the address of a lazily-built, memoized vtable (see
            // `vtable_for`). `node.r#type` is always `SpecObject` here
            // (`Analyzer::coerce_to_expected` guarantees it); `base`'s own
            // type is always a plain `Pointer` to the concrete struct/enum/
            // union that vtable is built for.
            MirExpr::SpecCoerce(MirSpecCoerce { base }) => {
                let ResolvedType::SpecObject { spec, .. } = &node.r#type else {
                    unreachable!("mir body guarantees a SpecCoerce's own type is SpecObject");
                };
                let spec = spec.clone();
                let ResolvedType::Pointer { pointee, .. } = &base.r#type else {
                    unreachable!("mir body guarantees a SpecCoerce's base is a plain pointer");
                };
                let concrete = (**pointee).clone();
                let data_ptr = self.process_expr(builder, *base)[0];
                let vtable_id = self.vtable_for(&concrete, &spec);
                let global_value = self.module.declare_data_in_func(vtable_id, builder.func);
                let vtable_ptr = builder.ins().global_value(self.pointer_type(), global_value);
                vec![data_ptr, vtable_ptr]
            }

            // `base.method(args)` through a `spec *Spec` value -- loads the
            // function pointer out of `base`'s own vtable leaf at
            // `slot_index * pointer_width` and calls through it, reusing
            // the exact same `call_indirect`/`make_function_sig` path
            // every ordinary call already goes through; only how the
            // callee address itself is obtained differs (a vtable load
            // here, `func_addr`/`get_place_value` for an ordinary call).
            // `self` is `base`'s own data-pointer leaf, prepended exactly
            // like an ordinary method call's own implicit self.
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
                // The one and only leaf of `node.r#type`'s own flattening --
                // every resolved numeric type is exactly one IR leaf --
                // picks the concrete width/kind to narrow `value` into.
                // `value` itself is already range-checked against this same
                // type by analysis, so this never has to reject anything,
                // only narrow losslessly.
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

            // `sizeof<Type>` -- a compile-time-known `usize` constant.
            // Fully general (unlike `sizeof<Type>` used *inside* an
            // `@layout` argument, which is scoped to primitives -- see
            // `ResolvedType::primitive_byte_size`'s doc comment): `Type`
            // may be any struct/enum/primitive, since `total_bytes` already
            // handles all of them uniformly.
            MirExpr::Sizeof(target_type) => {
                let size = layout::total_bytes(&target_type, self.pointer_bytes());
                vec![builder.ins().iconst(self.pointer_type(), size as i64)]
            }

            // Cranelift has no dedicated char/codepoint type -- a `char`'s
            // one IR leaf is just its `u32` codepoint stored in an `I32`
            // (see `Char`'s leaf-flattening arm).
            MirExpr::Char(c) => vec![builder.ins().iconst(types::I32, c as i64)],

            MirExpr::Place(place) => self.get_place_value(&place, builder),

            MirExpr::Assignment(MirAssignment { target, value }) => {
                let values = self.process_expr(builder, *value);
                // Uniformly covers assignment to a local, through any depth
                // of explicit/seamless deref (`*ptr = 5;`, `ptr.field = 5;`),
                // and through array indexing -- whatever `target` resolved
                // to, `store_scalars` only cares whether it has an address
                // (`todo!()`s itself for the one case that doesn't yet, a
                // parameter with no deref in between).
                let (storage, _) = self.resolve_place_storage(&target, builder);
                self.store_scalars(builder, &storage, &values);
                values
            }

            MirExpr::AddressOf(MirAddressOf { place }) => {
                let (storage, _) = self.resolve_place_storage(&place, builder);
                vec![self.place_storage_address(builder, &storage)]
            }

            MirExpr::Negate(base) => {
                // The mir guarantees only signed ints or floats reach here
                // -- `fneg` for the latter, `ineg` (two's-complement
                // negation) for the former.
                let is_float = matches!(base.r#type.numeric_kind(), Some(NumericKind::Float(_)));
                let value = self.process_expr(builder, *base)[0];
                let result = if is_float { builder.ins().fneg(value) } else { builder.ins().ineg(value) };
                vec![result]
            }

            MirExpr::BitNot(base) => {
                // The mir guarantees only signed/unsigned integers reach
                // here.
                let value = self.process_expr(builder, *base)[0];
                vec![builder.ins().bnot(value)]
            }

            MirExpr::BinaryOp(MirBinaryOp { op, left, right }) => {
                // The mir guarantees both operands share the same resolved
                // type, so either one's `numeric_kind` picks the right
                // instruction for the whole operation. `Char` is the one
                // exception: it has no `numeric_kind` of its own (see its
                // doc comment -- arithmetic/bitwise on it is meaningless,
                // possibly UTF-8-breaking), but analysis only ever lets it
                // reach here for a *comparison* op, where it behaves
                // exactly like its underlying representation: an unsigned
                // 4-byte scalar, ordered by codepoint.
                let kind = match &left.r#type {
                    ResolvedType::Char => NumericKind::Unsigned(32),
                    r#type => r#type
                        .numeric_kind()
                        .expect("mir body guarantees BinaryOp operands are numeric or char"),
                };
                let left = self.process_expr(builder, *left)[0];
                let right = self.process_expr(builder, *right)[0];
                // Division/modulo by zero traps at the instruction level --
                // consistent with this language having no other runtime
                // safety net (no bounds checks either), so no special
                // handling is needed here.
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
                    // The mir guarantees neither operand is a float for any
                    // of these -- signedness never matters except for
                    // `>>`, which needs to pick arithmetic (sign-extending)
                    // vs. logical shift.
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
                // Each element contributes its own leaves, in order -- the
                // exact flattening a `SizedArray`'s own leaves expect, so
                // the result is usable anywhere a `SizedArray` value
                // already is (assignment, a walrus's inferred value, ...).
                elements.into_iter().flat_map(|e| self.process_expr(builder, e)).collect()
            }

            MirExpr::EnumConstruct(MirEnumConstruct { variant_index, fields }) => {
                // Built in an anonymous scratch slot -- constants (tag,
                // header), the shared dynamic fields, and typed body fields
                // all land at their byte offsets, the rest of the payload
                // region is zeroed (deterministic bytes for the chunk-wise
                // copies the leaf model does; the dynamic-fields region
                // needs no such zeroing -- every dynamic field is always
                // supplied by `fields` below, unlike the payload's union
                // slack) -- then the whole value is read back out as
                // ordinary leaves.
                let ResolvedType::Enum { cell, .. } = &node.r#type else {
                    unreachable!("mir body guarantees a construction's own type is its enum");
                };
                let cell = cell.clone();
                let pointer_bytes = self.pointer_bytes();
                // Snapshot everything needed so the cell isn't borrowed
                // across the field-value evaluation below.
                let (tag, tag_type, header, payload_offset, chunk_leaves, field_offsets) = {
                    let enum_type = cell.borrow();
                    let variant = &enum_type.variants[variant_index];
                    let header: Vec<(ResolvedType, ConstValue)> = enum_type
                        .header
                        .iter()
                        .zip(&variant.header_values)
                        .map(|((_, r#type, _), value)| (r#type.clone(), value.clone()))
                        .collect();
                    // `field.field_index` (from `MirEnumConstruct::fields`)
                    // spans the *combined* declared list analysis built --
                    // shared dynamic fields first, then this variant's own
                    // body fields -- so this offset table is built in that
                    // exact same order.
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

                // Dynamic and body field values run in source order (their
                // side effects must); each lands at its declared field's
                // offset.
                for field in fields {
                    let field_offset = field_offsets[field.field_index];
                    let values = self.process_expr(builder, field.value);
                    self.store_scalars(builder, &PlaceStorage::Slot { slot, offset: field_offset }, &values);
                }

                self.load_scalars(builder, &PlaceStorage::Slot { slot, offset: 0 }, &node.r#type)
            }

            MirExpr::StructLiteral(MirStructLiteral { fields }) => {
                // Values are evaluated in the order the user wrote them
                // (their side effects must run in source order), but the
                // result's leaves are concatenated in *declared field*
                // order -- the exact flattening a struct's own leaves
                // expect, so the result is usable anywhere a struct value
                // already is. The mir guarantees every declared field
                // appears exactly once.
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

                // A slice's data pointer and full length, however `base` is
                // actually stored: a `SizedArray`'s elements live inline, so
                // the pointer is the storage's own address and the length is
                // a compile-time constant; a `Slice`/`Str` already carries
                // both as its two flattened leaves (identical layout for
                // both -- re-slicing a `*str` produces another `*str`,
                // decided by `node.r#type` above this match, not by
                // anything read here).
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
                    _ => unreachable!("mir body guarantees a slice's base is SizedArray/Slice/Str"),
                };

                let elem_size = layout::total_bytes(&item_type, self.pointer_bytes()) as i64;

                let start_val = match start {
                    Some(e) => self.process_expr(builder, *e)[0],
                    None => builder.ins().iconst(types::I32, 0),
                };
                // An inclusive end (`...`) with an explicit bound includes
                // that element itself, so it's one past `end` in the
                // exclusive terms the rest of this function computes in; an
                // absent end always means "through the real end of `base`"
                // regardless of `inclusive` -- there's nothing to be
                // exclusive *of* when there's no bound at all.
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
                // Captures every leaf, not just the first -- `Reinterpret`
                // needs all of them (a fat pointer's `[ptr, len]` passed
                // through unchanged, same as a numeric cast's own single
                // leaf passed through unchanged); every other `CastKind`
                // only ever applies to a single-leaf numeric source (`Str`/
                // `Slice` never reach them -- `cast_class` stays `None` for
                // both, so `byte_pointer_cast_kind` always intercepts
                // first), so indexing `[0]` for those is still exactly
                // right.
                let base_leaves = self.process_expr(builder, *base);
                let target_ir = target_type.cranelift_leaves(self)[0];
                match kind {
                    CastKind::Reinterpret => base_leaves,
                    CastKind::DropLength => vec![base_leaves[0]],
                    CastKind::IntExtend { signed: true } => vec![builder.ins().sextend(target_ir, base_leaves[0])],
                    CastKind::IntExtend { signed: false } => vec![builder.ins().uextend(target_ir, base_leaves[0])],
                    CastKind::IntTruncate => vec![builder.ins().ireduce(target_ir, base_leaves[0])],
                    CastKind::IntToFloat { signed: true } => vec![builder.ins().fcvt_from_sint(target_ir, base_leaves[0])],
                    CastKind::IntToFloat { signed: false } => vec![builder.ins().fcvt_from_uint(target_ir, base_leaves[0])],
                    CastKind::FloatToInt { signed: true } => vec![builder.ins().fcvt_to_sint_sat(target_ir, base_leaves[0])],
                    CastKind::FloatToInt { signed: false } => vec![builder.ins().fcvt_to_uint_sat(target_ir, base_leaves[0])],
                    CastKind::FloatExtend => vec![builder.ins().fpromote(target_ir, base_leaves[0])],
                    CastKind::FloatTruncate => vec![builder.ins().fdemote(target_ir, base_leaves[0])],
                }
            }

            MirExpr::UnionConstruct(MirUnionConstruct { field_index: _, value }) => {
                // Mirrors `EnumConstruct`'s shape (anonymous slot, zero the
                // whole region deterministically, store the one field's
                // scalars, read the whole thing back as flattened leaves) --
                // minus the tag/header steps, since a union has neither.
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
