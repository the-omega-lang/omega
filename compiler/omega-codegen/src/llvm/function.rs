use super::Codegen;
use super::leaf;
use crate::abi::{AbiReturn, AbiSignature};

use inkwell::module::Linkage;
use inkwell::types::{BasicType, BasicTypeEnum};
use inkwell::values::{BasicValueEnum, PointerValue};
use omega_analyzer::checked::ExternFunctionRef;
use omega_analyzer::layout;
use omega_analyzer::resolved_type::{ResolvedFunctionType, ResolvedType};
use omega_mir::{MirExternDeclaration, MirFunctionDef, MirTerminator};

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
        self.declared_symbols
            .insert(symbol.clone(), function_def.id);

        let fn_type = self.llvm_function_type(&function_def.fn_type());
        let function = self.module.add_function(&symbol, fn_type, None);
        function.set_linkage(match function_def.linkage {
            omega_mir::MirLinkage::Export => Linkage::External,
            omega_mir::MirLinkage::Weak => Linkage::WeakODR,
        });
        // Keep each function in its own section so linker GC can reclaim dead code.
        if self.target.os != omega_analyzer::Os::MacOs {
            function.set_section(Some(&format!(".text.{symbol}")));
        }
        self.functions.insert(function_def.id, function);
    }

    pub(super) fn declare_extern_function(&mut self, extern_fn: &ExternFunctionRef) {
        let symbol = omega_mir::mangle::extern_function_ref_symbol(extern_fn);
        let fn_type = self.llvm_function_type(&extern_fn.fn_type);
        let function = self.module.add_function(&symbol, fn_type, None);
        function.set_linkage(Linkage::External);
        self.functions.insert(extern_fn.decl_id, function);
    }

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

    pub(super) fn define_function_def(&mut self, function_def: MirFunctionDef) {
        if self.symbol_error.is_some() {
            return;
        }

        let function = *self
            .functions
            .get(&function_def.id)
            .expect("declared for every item, across every module, before any body is defined");
        let MirFunctionDef {
            return_type, body, ..
        } = function_def;

        self.arg_count = body.arg_count;
        self.local_args = vec![Vec::new(); body.locals.len()];

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

        // Pack non-parameter locals into one shared frame so zero-sized offsets match shared layout.
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
