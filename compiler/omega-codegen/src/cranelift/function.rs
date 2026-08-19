use super::Codegen;
use super::leaf::{IntoCraneliftLeaves, cranelift_type};
use crate::abi::{AbiReturn, AbiSignature};
use crate::storage::{ParameterHome, parameter_storage_plan};
use cranelift::codegen;
use cranelift::codegen::ir::ArgumentPurpose;
use cranelift::prelude::{
    AbiParam, FunctionBuilder, FunctionBuilderContext, Signature, StackSlotData, StackSlotKind, isa,
};
use cranelift_module::{Linkage, Module};
use omega_analyzer::checked::ExternFunctionRef;
use omega_analyzer::layout;
use omega_analyzer::resolved_type::{ResolvedFunctionType, ResolvedType};
use omega_mir::{MirExternDeclaration, MirFunctionDef, MirTerminator};

impl Codegen {
    pub(super) fn needs_sret(&self, return_type: &ResolvedType) -> bool {
        matches!(
            AbiReturn::for_type(self.target, return_type),
            AbiReturn::Indirect
        )
    }

    pub(super) fn make_function_sig(&self, resolved_fntype: ResolvedFunctionType) -> Signature {
        // Translate the shared ABI signature directly into Cranelift types.
        let abi = AbiSignature::build(self.target, &resolved_fntype);
        let mut sig = self.module.make_signature();

        match abi.ret {
            // Indirect returns receive the hidden sret pointer as the first ABI parameter.
            AbiReturn::Void => {}
            AbiReturn::Indirect => {
                sig.params.push(AbiParam::special(
                    self.pointer_type(),
                    ArgumentPurpose::StructReturn,
                ));
            }
            AbiReturn::Direct(leaves) => {
                for leaf in leaves {
                    sig.returns
                        .push(AbiParam::new(cranelift_type(leaf, self.pointer_type())));
                }
            }
        }

        for leaf in abi.params {
            sig.params
                .push(AbiParam::new(cranelift_type(leaf, self.pointer_type())));
        }

        if resolved_fntype.is_variadic {
            sig.call_conv = isa::CallConv::SystemV;
        }

        sig
    }

    pub(super) fn function_signature(&self, function_def: &MirFunctionDef) -> Signature {
        self.make_function_sig(function_def.fn_type())
    }

    pub(super) fn update_extern_decl(&mut self, extern_decl: MirExternDeclaration) {
        match extern_decl.r#type {
            ResolvedType::Function(resolved_fntype) => {
                // Consume the MIR-provided symbol; backend code must not reinterpret mangling policy.
                let sig = self.make_function_sig(resolved_fntype);

                let function_id = self
                    .module
                    .declare_function(&extern_decl.symbol, Linkage::Import, &sig)
                    .unwrap();

                self.functions.insert(extern_decl.id, function_id);
            }

            _ => unreachable!(
                "extern data declarations are rejected by the shared preflight (crate::preflight) before any backend runs"
            ),
        }
    }

    pub(super) fn declare_function_def(
        &mut self,
        function_def: &MirFunctionDef,
        linkage: Linkage,
    ) -> Result<(), String> {
        let symbol = &function_def.symbol;
        self.symbols.register_function(symbol, function_def.id)?;
        let sig = self.function_signature(function_def);
        let function_id = self
            .module
            .declare_function(symbol, linkage, &sig)
            .map_err(|error| format!("failed to declare function '{symbol}': {error}"))?;
        self.functions.insert(function_def.id, function_id);
        Ok(())
    }

    pub(super) fn declare_extern_function(&mut self, extern_fn: &ExternFunctionRef) {
        // Extern references use the MIR-computed symbol shared by both backends.
        let mangled = omega_mir::mangle::extern_function_ref_symbol(extern_fn);
        let sig = self.make_function_sig(extern_fn.fn_type.clone());

        let function_id = self
            .module
            .declare_function(&mangled, Linkage::Import, &sig)
            .unwrap();
        self.functions.insert(extern_fn.decl_id, function_id);
    }

    pub(super) fn define_function_def(&mut self, function_def: MirFunctionDef) {
        let function_id = *self
            .functions
            .get(&function_def.id)
            .expect("declared for every item, across every module, before any body is defined");
        let sig = self.function_signature(&function_def);
        let MirFunctionDef {
            return_type, body, ..
        } = function_def;
        let parameter_storage = parameter_storage_plan(&body);

        // Move the function context out temporarily so the builder does not block borrowing other Codegen state.
        let mut ctx = std::mem::replace(&mut self.ctx, codegen::Context::new());
        // Re-enable disassembly per function because clearing the context resets that flag.
        ctx.set_disasm(self.emit == crate::EmitKind::Asm);
        let mut fbctx = FunctionBuilderContext::new();
        let mut builder = FunctionBuilder::new(&mut ctx.func, &mut fbctx);
        builder.func.signature = sig;

        self.arg_count = body.arg_count;
        self.local_args = vec![Vec::new(); body.locals.len()];
        self.parameter_slots = vec![None; body.arg_count];

        // Pack non-parameter locals into one frame slot so zero-sized offsets match shared layout.
        let non_param_types: Vec<ResolvedType> = body.locals[body.arg_count..]
            .iter()
            .map(|local| local.r#type.clone())
            .collect();
        let frame = layout::locals_layout(&non_param_types, self.pointer_bytes());
        let max_align = non_param_types
            .iter()
            .map(|ty| layout::type_alignment(ty))
            .max()
            .unwrap_or(1);
        let shift = layout::stack_align_shift(max_align);
        self.frame_slot = Some(builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            frame.packed_end,
            shift,
        )));
        let mut local_offsets = vec![0u32; body.locals.len()];
        local_offsets[body.arg_count..].copy_from_slice(&frame.byte_offsets);
        self.local_offsets = local_offsets;

        // Create all backend blocks up front so forward and back edges always resolve.
        let cranelift_blocks: Vec<cranelift::prelude::Block> =
            body.blocks.iter().map(|_| builder.create_block()).collect();
        let entry_block = cranelift_blocks[0];

        builder.append_block_params_for_function_params(entry_block);
        let block_params = builder.block_params(entry_block).to_vec();

        // Remove the hidden sret parameter before mapping declared parameters.
        let sret = self.needs_sret(&return_type).then(|| block_params[0]);
        let declared_params = &block_params[sret.is_some() as usize..];

        // Map each flattened ABI leaf back to its originating MIR parameter.
        let argmap: Vec<usize> = body.locals[..body.arg_count]
            .iter()
            .enumerate()
            .flat_map(|(i, local)| {
                let value_count = local.r#type.cranelift_leaves(self).len();
                vec![i; value_count]
            })
            .collect();
        for (i, arg) in declared_params.iter().enumerate() {
            self.local_args[argmap[i]].push(*arg);
        }

        builder.switch_to_block(entry_block);
        for (index, home) in parameter_storage.into_iter().enumerate() {
            if home == ParameterHome::Ssa {
                continue;
            }
            let parameter_type = &body.locals[index].r#type;
            let slot = builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                layout::total_bytes(parameter_type, self.pointer_bytes()).max(1),
                layout::stack_align_shift(layout::type_alignment(parameter_type)),
            ));
            let values = self.local_args[index].clone();
            self.store_scalars(
                &mut builder,
                &super::place::PlaceStorage::Slot { slot, offset: 0 },
                &values,
            );
            self.parameter_slots[index] = Some(slot);
        }

        for (block_index, (mir_block, &cranelift_block)) in body
            .blocks
            .into_iter()
            .zip(&cranelift_blocks)
            .enumerate()
        {
            // The entry block is already current because parameter homes are initialized
            // above. Switching to it again after emitting a spill store violates
            // FunctionBuilder's rule that the current block must be terminated first.
            if block_index != 0 {
                builder.switch_to_block(cranelift_block);
            }
            for stmt in mir_block.statements {
                self.process_expr(&mut builder, stmt);
            }
            self.emit_terminator(&mut builder, mir_block.terminator, &cranelift_blocks, sret);
        }

        // Seal blocks only after all terminators are emitted and predecessor sets are final.
        for block in cranelift_blocks {
            builder.seal_block(block);
        }

        if let Err(err) = codegen::verify_function(builder.func, self.isa.as_ref()) {
            panic!(
                "cranelift verifier rejected generated IR for a function (internal codegen bug): {err:?}"
            );
        }

        builder.finalize();

        self.module.define_function(function_id, &mut ctx).unwrap();

        // Read CLIF/disassembly after definition while the same context still owns both results.
        match self.emit {
            crate::EmitKind::Ir => {
                let name = self
                    .module
                    .declarations()
                    .get_function_decl(function_id)
                    .name
                    .clone()
                    .unwrap_or_default();
                self.captured_text
                    .push_str(&format!("; {name}\n{}\n\n", ctx.func));
            }
            crate::EmitKind::Asm => {
                let name = self
                    .module
                    .declarations()
                    .get_function_decl(function_id)
                    .name
                    .clone()
                    .unwrap_or_default();
                let vcode = ctx
                    .compiled_code()
                    .and_then(|c| c.vcode.clone())
                    .unwrap_or_default();
                self.captured_text
                    .push_str(&format!("; {name}\n{vcode}\n\n"));
            }
            crate::EmitKind::Obj => {}
        }

        self.ctx = ctx;

        self.clear_local();
    }

    fn emit_terminator(
        &mut self,
        builder: &mut FunctionBuilder,
        terminator: MirTerminator,
        cranelift_blocks: &[cranelift::prelude::Block],
        sret: Option<cranelift::prelude::Value>,
    ) {
        use cranelift::prelude::{InstBuilder, TrapCode};
        match terminator {
            MirTerminator::Goto(target) => {
                builder.ins().jump(cranelift_blocks[target.index()], &[]);
            }
            MirTerminator::Branch {
                condition,
                then_block,
                else_block,
            } => {
                let cond_value = self.process_expr(builder, condition)[0];
                builder.ins().brif(
                    cond_value,
                    cranelift_blocks[then_block.index()],
                    &[],
                    cranelift_blocks[else_block.index()],
                    &[],
                );
            }
            MirTerminator::Return(value) => {
                let leaves = value
                    .map(|v| self.process_expr(builder, v))
                    .unwrap_or_default();
                // Store indirect-return leaves through sret; otherwise return leaves directly.
                match sret {
                    Some(pointer) => {
                        self.store_scalars(
                            builder,
                            &super::place::PlaceStorage::Address {
                                base: pointer,
                                offset: 0,
                            },
                            &leaves,
                        );
                        builder.ins().return_(&[]);
                    }
                    None => {
                        builder.ins().return_(&leaves);
                    }
                }
            }
            MirTerminator::Unreachable => {
                builder.ins().trap(TrapCode::unwrap_user(1));
            }
        }
    }
}
