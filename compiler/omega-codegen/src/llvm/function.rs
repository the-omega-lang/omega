//! Building an LLVM function type from the shared ABI (the exact
//! counterpart of `cranelift/function.rs`'s `make_function_sig`), and
//! declaring/defining a function's own body.

use super::leaf;
use super::Codegen;
use crate::abi::{AbiReturn, AbiSignature};

use inkwell::module::Linkage;
use inkwell::types::{BasicType, BasicTypeEnum};
use inkwell::values::{BasicValueEnum, PointerValue};
use omega_analyzer::checked::ExternFunctionRef;
use omega_analyzer::layout;
use omega_analyzer::resolved_type::{ResolvedFunctionType, ResolvedType};
use omega_mir::{MirExternDeclaration, MirFunctionDef, MirTerminator};

impl<'ctx> Codegen<'ctx> {
    /// Whether `return_type` is returned through a hidden struct-return
    /// pointer instead of in registers -- the shared ABI's answer, same
    /// call the Cranelift backend makes.
    pub(super) fn needs_sret(&self, return_type: &ResolvedType) -> bool {
        matches!(AbiReturn::for_type(self.target, return_type), AbiReturn::Indirect)
    }

    /// The LLVM function type for one `ResolvedFunctionType` -- a pure
    /// translation of `AbiSignature` into LLVM types, built identically
    /// for definitions, externs, and call sites (call sites don't *use*
    /// this directly -- they call through `FunctionValue`s whose type was
    /// built here -- but the single builder keeps every consumer on the
    /// same convention).
    ///
    /// - `Direct` returns become one scalar (single leaf) or an aggregate
    ///   struct of the leaves (LLVM's own ABI lowering flattens that
    ///   aggregate into the same rax/rdx pair Cranelift's two-return-value
    ///   lowering produces -- both follow the same SysV rule).
    /// - `Indirect` adds a hidden `sret` pointer parameter; the return
    ///   type is `void`, and LLVM's ABI lowering handles the SysV "return
    ///   the pointer in rax" requirement from the attribute alone, exactly
    ///   like Cranelift's `ArgumentPurpose::StructReturn`.
    /// - `Void`/`Never` are plain `void`.
    pub(super) fn llvm_function_type(&self, fn_type: &ResolvedFunctionType) -> inkwell::types::FunctionType<'ctx> {
        let abi = AbiSignature::build(self.target, fn_type);
        let mut param_types: Vec<BasicTypeEnum> = Vec::new();
        if matches!(abi.ret, AbiReturn::Indirect) {
            param_types.push(self.ptr_type().as_basic_type_enum());
        }
        let pb = self.pointer_bytes();
        param_types.extend(abi.params.iter().map(|raw_leaf| leaf::llvm_type(self.context, *raw_leaf, pb)));
        let param_types: Vec<inkwell::types::BasicMetadataTypeEnum> =
            param_types.into_iter().map(Into::into).collect();
        match &abi.ret {
            AbiReturn::Void | AbiReturn::Indirect => self
                .context
                .void_type()
                .fn_type(&param_types, fn_type.is_variadic),
            AbiReturn::Direct(leaves) => match leaves.as_slice() {
                [single] => leaf::llvm_type(self.context, *single, pb).fn_type(&param_types, fn_type.is_variadic),
                multiple => self
                    .context
                    .struct_type(
                        &multiple
                            .iter()
                            .map(|raw_leaf| leaf::llvm_type(self.context, *raw_leaf, pb))
                            .collect::<Vec<_>>(),
                        false,
                    )
                    .fn_type(&param_types, fn_type.is_variadic),
            },
        }
    }

    /// Declares one local function/method -- `Linkage::External` (strong)
    /// for `MirLinkage::Export`, `Linkage::Weak` for `MirLinkage::Weak`,
    /// the same strong/weak distinction `cranelift::item` makes. The
    /// symbol-collision guard mirrors the Cranelift backend's
    /// `declare_function_def` exactly (same message, same dedup trick).
    pub(super) fn declare_function_def(&mut self, function_def: &MirFunctionDef) {
        let symbol = function_def.symbol.clone();
        if let Some(&existing_id) = self.declared_symbols.get(&symbol)
            && existing_id != function_def.id
        {
            self.symbol_error.get_or_insert_with(|| {
                format!(
                    "two different functions both produce the linker symbol '{symbol}' -- this can \
                     happen when '@mangling(disabled)' is used on more than one function with the same name, \
                     or when '@mangling(force = \"...\")' gives two different functions the same forced name; \
                     give one of them a different name, or re-enable mangling"
                )
            });
            return;
        }
        self.declared_symbols.insert(symbol.clone(), function_def.id);

        let fn_type = self.llvm_function_type(&function_def.fn_type());
        let function = self.module.add_function(&symbol, fn_type, None);
        function.set_linkage(match function_def.linkage {
            omega_mir::MirLinkage::Export => Linkage::External,
            omega_mir::MirLinkage::Weak => Linkage::WeakODR,
        });
        // One function per section, so `--gc-sections` can reclaim dead
        // copies -- the LLVM counterpart of Cranelift's
        // `per_function_section(true)`. (Mach-O uses subsections-via-
        // symbols instead; its own conventions are the one place this
        // naming is skipped -- see `docs/16`.)
        if self.target.os != omega_analyzer::Os::MacOs {
            function.set_section(Some(&format!(".text.{symbol}")));
        }
        self.functions.insert(function_def.id, function);
    }

    /// Declares a link against an extern-owned function/method --
    /// `Linkage::External` declaration only, no body. The symbol comes
    /// from the shared `omega_mir::mangle::extern_function_ref_symbol`.
    pub(super) fn declare_extern_function(&mut self, extern_fn: &ExternFunctionRef) {
        let symbol = omega_mir::mangle::extern_function_ref_symbol(extern_fn);
        let fn_type = self.llvm_function_type(&extern_fn.fn_type);
        let function = self.module.add_function(&symbol, fn_type, None);
        function.set_linkage(Linkage::External);
        self.functions.insert(extern_fn.decl_id, function);
    }

    /// Externs have no body -- fully handled by the declare pass
    /// (`cranelift::update_extern_decl`'s LLVM counterpart; the symbol is
    /// MIR-carried).
    pub(super) fn declare_extern_decl(&mut self, extern_decl: &MirExternDeclaration) {
        let ResolvedType::Function(resolved_fntype) = &extern_decl.r#type else {
            unreachable!(
                "extern data declarations are rejected by the shared preflight (crate::preflight) before any backend runs"
            );
        };
        let fn_type = self.llvm_function_type(resolved_fntype);
        let function = self.module.add_function(&extern_decl.symbol, fn_type, None);
        function.set_linkage(Linkage::External);
        self.functions.insert(extern_decl.id, function);
    }

    /// Builds a function/method's body -- the same shape as
    /// `cranelift/function.rs`'s `define_function_def`: one LLVM block
    /// per `MirBlockData`, then translate each one's statements and its
    /// single terminator. There is no control-flow bookkeeping here -- the
    /// mir body already *is* the control-flow graph.
    pub(super) fn define_function_def(&mut self, function_def: MirFunctionDef) {
        if self.symbol_error.is_some() {
            return;
        }

        let function = *self
            .functions
            .get(&function_def.id)
            .expect("declared for every item, across every module, before any body is defined");
        let MirFunctionDef { return_type, body, .. } = function_def;

        self.arg_count = body.arg_count;
        self.local_args = vec![Vec::new(); body.locals.len()];

        // One combined alloca for every non-parameter local, laid out by
        // `layout::locals_layout` -- the exact counterpart of Cranelift's
        // `frame_slot` (see its doc comment for why one shared slot, not
        // one per local).
        let non_param_types: Vec<ResolvedType> = body.locals[body.arg_count..]
            .iter()
            .map(|local| local.r#type.clone())
            .collect();
        let frame = layout::locals_layout(&non_param_types, self.pointer_bytes());
        let max_align = non_param_types
            .iter()
            .map(layout::type_alignment)
            .max()
            .unwrap_or(1);

        // Entry block first, in MIR order, so a forward reference (or a
        // loop's back-edge) always resolves regardless of fill order.
        let blocks: Vec<inkwell::basic_block::BasicBlock<'ctx>> = body
            .blocks
            .iter()
            .enumerate()
            .map(|(i, _)| {
                self.context
                    .append_basic_block(function, &format!("block{i}"))
            })
            .collect();

        // Every `alloca` this backend emits goes in the entry block, not
        // just this one -- see `Codegen::entry_alloca`.
        self.entry_block = Some(blocks[0]);
        self.builder.position_at_end(blocks[0]);

        // One combined alloca for every non-parameter local, laid out by
        // `layout::locals_layout` -- the exact counterpart of Cranelift's
        // `frame_slot` (see its doc comment for why one shared slot, not
        // one per local).
        //
        // `stack_align_shift` answers in *shift* units (a backend
        // stack-slot API's own currency); LLVM wants bytes, so `1 <<
        // shift`, exactly like every other `entry_alloca` caller.
        let frame_slot = if frame.packed_end == 0 {
            None
        } else {
            Some(self.entry_alloca(
                frame.packed_end,
                1u32 << layout::stack_align_shift(max_align),
                "locals",
            ))
        };
        self.frame_slot = frame_slot;
        let mut local_offsets = vec![0u32; body.locals.len()];
        local_offsets[body.arg_count..].copy_from_slice(&frame.byte_offsets);
        self.local_offsets = local_offsets;

        // Seed parameter SSA values from the function's own parameters --
        // the sret pointer (when present) is the signature's first
        // parameter and is peeled off before mapping the declared ones,
        // exactly like `cranelift/function.rs`.
        let params: Vec<BasicValueEnum> = function.get_params().into_iter().collect();
        let sret_offset = usize::from(self.needs_sret(&return_type));
        let declared_params = &params[sret_offset..];
        let argmap: Vec<usize> = body.locals[..body.arg_count]
            .iter()
            .enumerate()
            .flat_map(|(i, local)| {
                let value_count = leaf::llvm_leaves(self.context, &local.r#type, self.pointer_bytes()).len();
                vec![i; value_count]
            })
            .collect();
        for (i, arg) in declared_params.iter().enumerate() {
            self.local_args[argmap[i]].push(*arg);
        }

        // The sret pointer, for the Return terminator below.
        let sret_ptr: Option<PointerValue> = self.needs_sret(&return_type).then(|| {
            params[0].into_pointer_value()
        });

        for (mir_block, &llvm_block) in body.blocks.iter().zip(&blocks) {
            self.builder.position_at_end(llvm_block);
            for stmt in &mir_block.statements {
                self.process_expr(&stmt.clone());
            }
            self.emit_terminator(mir_block.terminator.clone(), &blocks, sret_ptr, &return_type);
        }

        self.clear_local();
    }

    /// Translates one `MirBlockData`'s single terminator into the LLVM
    /// instruction(s) that end its block -- `sret`, when `Some`, is the
    /// hidden struct-return pointer every `Return` stores its value
    /// through (see `llvm_function_type`'s doc comment).
    fn emit_terminator(
        &mut self,
        terminator: MirTerminator,
        blocks: &[inkwell::basic_block::BasicBlock<'ctx>],
        sret: Option<PointerValue<'ctx>>,
        return_type: &ResolvedType,
    ) {
        match terminator {
            MirTerminator::Goto(target) => {
                self.builder
                    .build_unconditional_branch(blocks[target.0 as usize])
                    .expect("builder positioned");
            }
            MirTerminator::Branch { condition, then_block, else_block } => {
                // Omega's `bool` is an `i8` (`Leaf::I8`); `br` wants an `i1`
                // -- see `expr::to_i1`.
                let cond = self.process_expr(&condition)[0].into_int_value();
                let cond = self.to_i1(cond);
                self.builder
                    .build_conditional_branch(cond, blocks[then_block.0 as usize], blocks[else_block.0 as usize])
                    .expect("builder positioned");
            }
            MirTerminator::Return(value) => {
                let leaves: Vec<BasicValueEnum> =
                    value.map(|v| self.process_expr(&v)).unwrap_or_default();
                match sret {
                    Some(pointer) => {
                        self.store_scalars(&pointer, 0, &leaves, layout::type_alignment(return_type));
                        self.builder.build_return(None).expect("builder positioned");
                    }
                    None => match leaves.as_slice() {
                        [] => {
                            self.builder.build_return(None).expect("builder positioned");
                        }
                        [single] => {
                            self.builder
                                .build_return(Some(single))
                                .expect("builder positioned");
                        }
                        multiple => {
                            // Aggregate-return: build the struct value from
                            // the leaves -- LLVM's ABI lowering flattens it
                            // into the same registers Cranelift's
                            // multi-value return uses.
                            let struct_type = self
                                .context
                                .struct_type(
                                    &multiple.iter().map(|v| v.get_type()).collect::<Vec<_>>(),
                                    false,
                                );
                            let mut agg = struct_type.const_zero().into();
                            for (i, leaf_value) in multiple.iter().enumerate() {
                                agg = self
                                    .builder
                                    .build_insert_value(agg, *leaf_value, i as u32, "")
                                    .expect("insertvalue on the return aggregate");
                            }
                            self.builder.build_return(Some(&agg)).expect("builder positioned");
                        }
                    },
                }
            }
            MirTerminator::Unreachable => {
                self.builder.build_unreachable().expect("builder positioned");
            }
        }
    }
}
