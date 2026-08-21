use super::Codegen;
use super::leaf;
use crate::abi::{AbiReturn, AbiSignature};
use crate::storage::{ParameterHome, parameter_storage_plan};

use inkwell::attributes::{Attribute, AttributeLoc};
use inkwell::module::Linkage;
use inkwell::types::{BasicType, BasicTypeEnum};
use inkwell::values::{BasicValueEnum, PointerValue};
use omega_analyzer::checked::ExternFunctionRef;
use omega_analyzer::layout;
use omega_analyzer::resolved_type::{ResolvedFunctionType, ResolvedType};
use omega_mir::{MirExternDeclaration, MirFunctionBody, MirFunctionDef, MirInlineAsm, MirTerminator};

impl<'ctx> Codegen<'ctx> {
    pub(super) fn needs_sret(&self, return_type: &ResolvedType) -> bool {
        matches!(
            AbiReturn::for_type(self.target, return_type),
            AbiReturn::Indirect
        )
    }

    pub(super) fn llvm_function_type(
        &self,
        fn_type: &ResolvedFunctionType,
    ) -> inkwell::types::FunctionType<'ctx> {
        let abi = AbiSignature::build(self.target, fn_type);
        let mut param_types: Vec<BasicTypeEnum> = Vec::new();
        if matches!(abi.ret, AbiReturn::Indirect) {
            param_types.push(self.ptr_type().as_basic_type_enum());
        }
        let pb = self.pointer_bytes();
        param_types.extend(
            abi.params
                .iter()
                .map(|raw_leaf| leaf::llvm_type(self.context, *raw_leaf, pb)),
        );
        let param_types: Vec<inkwell::types::BasicMetadataTypeEnum> =
            param_types.into_iter().map(Into::into).collect();
        match &abi.ret {
            AbiReturn::Void | AbiReturn::Indirect => self
                .context
                .void_type()
                .fn_type(&param_types, fn_type.is_variadic),
            AbiReturn::Direct(leaves) => match leaves.as_slice() {
                [single] => leaf::llvm_type(self.context, *single, pb)
                    .fn_type(&param_types, fn_type.is_variadic),
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

    /// A gap declaration and its matching glue definition intentionally mangle to the same
    /// linker symbol (see `docs/architecture/symbol-mangling.md`, "Gap/glue identity"), so two
    /// distinct `HirId`s can request the same LLVM function. `Module::add_function` does not
    /// check for an existing global of that name -- it always creates a new one and lets LLVM's
    /// symbol table silently rename the second to `<symbol>.1`, leaving whichever declaration
    /// callers actually reference without a body. Reuse the existing global instead.
    fn declare_or_reuse_function(
        &mut self,
        symbol: &str,
        fn_type: inkwell::types::FunctionType<'ctx>,
    ) -> (inkwell::values::FunctionValue<'ctx>, bool) {
        match self.module.get_function(symbol) {
            Some(existing) => (existing, false),
            None => (self.module.add_function(symbol, fn_type, None), true),
        }
    }

    pub(super) fn declare_function_def(
        &mut self,
        function_def: &MirFunctionDef,
    ) -> Result<(), String> {
        let symbol = &function_def.symbol;
        self.symbols.register_function(symbol, function_def.id)?;
        let fn_type = self.llvm_function_type(&function_def.fn_type());
        let (function, _) = self.declare_or_reuse_function(symbol, fn_type);
        // This item owns the body about to be attached, so its linkage always wins over
        // whatever a same-symbol extern/gap declaration set.
        function.set_linkage(match function_def.linkage {
            omega_mir::MirLinkage::Export => Linkage::External,
            omega_mir::MirLinkage::Weak => Linkage::WeakODR,
        });
        if self.target.os != omega_analyzer::Os::MacOs {
            function.set_section(Some(&format!(".text.{symbol}")));
        }
        if matches!(function_def.body, MirFunctionBody::Naked(_)) {
            // `naked` disables prologue/epilogue emission and forbids IR
            // references to function arguments; `noinline` is implied because
            // an inliner has no legal way to splice a naked body into a
            // caller. Both are semantic requirements of `@naked`, not hints.
            self.add_function_enum_attribute(function, "naked");
            self.add_function_enum_attribute(function, "noinline");
        }
        self.functions.insert(function_def.id, function);
        Ok(())
    }

    fn add_function_enum_attribute(
        &self,
        function: inkwell::values::FunctionValue<'ctx>,
        name: &str,
    ) {
        let kind_id = Attribute::get_named_enum_kind_id(name);
        let attribute = self.context.create_enum_attribute(kind_id, 0);
        function.add_attribute(AttributeLoc::Function, attribute);
    }

    pub(super) fn declare_extern_function(&mut self, extern_fn: &ExternFunctionRef) {
        let symbol = omega_mir::mangle::extern_function_ref_symbol(extern_fn);
        let fn_type = self.llvm_function_type(&extern_fn.fn_type);
        let (function, created) = self.declare_or_reuse_function(&symbol, fn_type);
        if created {
            function.set_linkage(Linkage::External);
        }
        self.functions.insert(extern_fn.decl_id, function);
    }

    pub(super) fn declare_extern_decl(&mut self, extern_decl: &MirExternDeclaration) {
        let ResolvedType::Function(resolved_fntype) = &extern_decl.r#type else {
            unreachable!(
                "extern data declarations are rejected by the shared preflight (crate::preflight) before any backend runs"
            );
        };
        let fn_type = self.llvm_function_type(resolved_fntype);
        let (function, created) = self.declare_or_reuse_function(&extern_decl.symbol, fn_type);
        if created {
            function.set_linkage(Linkage::External);
        }
        self.functions.insert(extern_decl.id, function);
    }

    pub(super) fn define_function_def(&mut self, function_def: MirFunctionDef) {
        let function = *self
            .functions
            .get(&function_def.id)
            .expect("declared for every item, across every module, before any body is defined");
        let MirFunctionDef {
            return_type, body, ..
        } = function_def;
        let body = match body {
            MirFunctionBody::Normal(body) => body,
            MirFunctionBody::Naked(asm) => return self.define_naked_function(function, &asm),
        };
        let parameter_storage = parameter_storage_plan(&body);

        self.arg_count = body.arg_count;
        self.local_args = vec![Vec::new(); body.locals.len()];
        self.parameter_slots = vec![None; body.arg_count];

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

        // Create all LLVM blocks before emission so forward and back edges always resolve.
        let blocks: Vec<inkwell::basic_block::BasicBlock<'ctx>> = body
            .blocks
            .iter()
            .enumerate()
            .map(|(i, _)| {
                self.context
                    .append_basic_block(function, &format!("block{i}"))
            })
            .collect();

        // Emit allocas in the entry block; loop-local allocas would grow the stack per iteration.
        self.entry_block = Some(blocks[0]);
        self.builder.position_at_end(blocks[0]);

        // Pack non-parameter locals into one shared frame so zero-sized offsets match shared
        // layout. Always allocate, even for a zero-size frame: a zero-sized local still needs a
        // stable address (`entry_alloca` rounds the byte count up to at least 1).
        self.frame_slot = Some(self.entry_alloca(
            frame.packed_end,
            1u32 << layout::stack_align_shift(max_align),
            "locals",
        ));
        let mut local_offsets = vec![0u32; body.locals.len()];
        local_offsets[body.arg_count..].copy_from_slice(&frame.byte_offsets);
        self.local_offsets = local_offsets;

        // Seed local parameter storage from the flattened ABI parameter sequence.
        let params: Vec<BasicValueEnum> = function.get_params().into_iter().collect();
        let sret_offset = usize::from(self.needs_sret(&return_type));
        let declared_params = &params[sret_offset..];
        let argmap: Vec<usize> = body.locals[..body.arg_count]
            .iter()
            .enumerate()
            .flat_map(|(i, local)| {
                let value_count =
                    leaf::llvm_leaves(self.context, &local.r#type, self.pointer_bytes()).len();
                vec![i; value_count]
            })
            .collect();
        for (i, arg) in declared_params.iter().enumerate() {
            self.local_args[argmap[i]].push(*arg);
        }

        for (index, home) in parameter_storage.into_iter().enumerate() {
            if home == ParameterHome::Ssa {
                continue;
            }
            let parameter_type = &body.locals[index].r#type;
            let align = layout::type_alignment(parameter_type);
            let slot = self.entry_alloca(
                layout::total_bytes(parameter_type, self.pointer_bytes()),
                align,
                "param",
            );
            let values = self.local_args[index].clone();
            self.store_scalars(&slot, 0, &values, align);
            self.parameter_slots[index] = Some(slot);
        }

        // Keep the hidden sret destination available for the final return terminator.
        let sret_ptr: Option<PointerValue> = self
            .needs_sret(&return_type)
            .then(|| params[0].into_pointer_value());

        for (mir_block, &llvm_block) in body.blocks.iter().zip(&blocks) {
            self.builder.position_at_end(llvm_block);
            for stmt in &mir_block.statements {
                self.process_expr(&stmt.clone());
            }
            self.emit_terminator(
                mir_block.terminator.clone(),
                &blocks,
                sret_ptr,
                &return_type,
            );
        }

        self.clear_local();
    }

    /// A naked function's body is exactly the side-effecting inline-asm call
    /// followed by `unreachable` -- LLVM requires a terminator even though
    /// the target asm itself owns control flow, and `naked` guarantees
    /// `unreachable` never becomes a real machine instruction. No parameter
    /// storage, locals, frame, or ordinary return terminator is created.
    fn define_naked_function(
        &mut self,
        function: inkwell::values::FunctionValue<'ctx>,
        asm: &MirInlineAsm,
    ) {
        let block = self.context.append_basic_block(function, "entry");
        self.builder.position_at_end(block);
        self.process_inline_asm(asm);
        self.builder
            .build_unreachable()
            .expect("builder positioned");
    }

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
                    .build_unconditional_branch(blocks[target.index()])
                    .expect("builder positioned");
            }
            MirTerminator::Branch {
                condition,
                then_block,
                else_block,
            } => {
                // Convert Omega `i8` booleans to LLVM `i1` branch conditions.
                let cond = self.process_expr(&condition)[0].into_int_value();
                let cond = self.to_i1(cond);
                self.builder
                    .build_conditional_branch(
                        cond,
                        blocks[then_block.index()],
                        blocks[else_block.index()],
                    )
                    .expect("builder positioned");
            }
            MirTerminator::Return(value) => {
                let leaves: Vec<BasicValueEnum> =
                    value.map(|v| self.process_expr(&v)).unwrap_or_default();
                match sret {
                    Some(pointer) => {
                        self.store_scalars(
                            &pointer,
                            0,
                            &leaves,
                            layout::type_alignment(return_type),
                        );
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
                            // Repack direct multi-leaf returns into LLVM's aggregate return value.
                            let struct_type = self.context.struct_type(
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
                            self.builder
                                .build_return(Some(&agg))
                                .expect("builder positioned");
                        }
                    },
                }
            }
            MirTerminator::Unreachable => {
                self.builder
                    .build_unreachable()
                    .expect("builder positioned");
            }
        }
    }
}
