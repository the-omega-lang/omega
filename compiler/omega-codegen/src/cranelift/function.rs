//! Building a Cranelift `Signature` for a function/method (shared by
//! definitions, externs, and call sites), and declaring/defining a
//! function's own symbol.

use super::Codegen;
use super::leaf::{IntoCraneliftLeaves, cranelift_type};
use crate::abi::{AbiReturn, AbiSignature};
use cranelift::codegen;
use cranelift::codegen::ir::ArgumentPurpose;
use cranelift::prelude::{
    AbiParam, FunctionBuilder, FunctionBuilderContext, Signature, StackSlotData, StackSlotKind, isa,
};
use cranelift_module::{FuncId, Linkage, Module};
use omega_analyzer::checked::ExternFunctionRef;
use omega_analyzer::layout;
use omega_analyzer::resolved_type::{ResolvedFunctionType, ResolvedType};
use omega_mir::{MirExternDeclaration, MirFunctionDef, MirTerminator};

impl Codegen {
    /// Whether `return_type` is returned through a hidden `StructReturn`
    /// pointer instead of in registers -- the shared ABI's answer
    /// (`crate::abi::AbiReturn::Indirect`), see `AbiReturn`'s doc comment
    /// for the rule. Must agree between definitions and call sites -- both
    /// derive their `Signature` from `make_function_sig`, so it's decided
    /// in exactly one place.
    pub(super) fn needs_sret(&self, return_type: &ResolvedType) -> bool {
        matches!(AbiReturn::for_type(self.target, return_type), AbiReturn::Indirect)
    }

    pub(super) fn make_function_sig(&self, resolved_fntype: ResolvedFunctionType) -> Signature {
        // The whole calling-convention shape comes from the shared ABI
        // (`crate::abi`) -- this function is now a pure *translation* of
        // that decision into Cranelift types.
        let abi = AbiSignature::build(self.target, &resolved_fntype);
        let mut sig = self.module.make_signature();

        match abi.ret {
            // The hidden struct-return pointer is always the first
            // parameter (see `AbiReturn::Indirect`); cranelift itself
            // handles the SysV requirement of also returning that pointer
            // in rax, so the signature declares no return values at all in
            // this case. `Never` takes the same empty-signature path as
            // `Void` -- see `AbiReturn::Void`'s doc comment.
            AbiReturn::Void => {}
            AbiReturn::Indirect => {
                sig.params.push(AbiParam::special(
                    self.pointer_type(),
                    ArgumentPurpose::StructReturn,
                ));
            }
            AbiReturn::Direct(leaves) => {
                for leaf in leaves {
                    sig.returns.push(AbiParam::new(cranelift_type(leaf, self.pointer_type())));
                }
            }
        }

        for leaf in abi.params {
            sig.params.push(AbiParam::new(cranelift_type(leaf, self.pointer_type())));
        }

        if resolved_fntype.is_variadic {
            sig.call_conv = isa::CallConv::SystemV;
        }

        sig
    }

    /// A function/method's cranelift `Signature`, built the same way
    /// regardless of whether it's being declared (pass 1) or defined (pass
    /// 2) -- and, crucially, the same way *call sites* build it: one
    /// delegation to `make_function_sig`, so the definition and every call
    /// can never disagree about parameter flattening or the hidden
    /// struct-return pointer.
    pub(super) fn function_signature(&self, function_def: &MirFunctionDef) -> Signature {
        self.make_function_sig(function_def.fn_type())
    }

    pub(super) fn update_extern_decl(&mut self, extern_decl: MirExternDeclaration) {
        match extern_decl.r#type {
            ResolvedType::Function(resolved_fntype) => {
                // The symbol was decided once, at lowering
                // (`MirExternDeclaration::symbol`); the `Disabled`/`Glued`
                // branches that used to live here moved to
                // `omega_mir::lower`.
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

    /// Declares (but doesn't yet define the body of) a function or method --
    /// signature/symbol registration only, split out from what used to be
    /// one `update_function_def` specifically so *every* function across
    /// *every* compiled module can be declared (and therefore have a
    /// `FuncId` any other module's body can already look up) before *any*
    /// body starts being built. Without this split, a cross-module call in
    /// either import direction would panic the first time one module's body
    /// needed another module's not-yet-declared `FuncId`.
    ///
    /// `linkage` is `Linkage::Export` (strong) for an ordinary item and
    /// `Linkage::Preemptible` (weak) for a generic instantiation (see
    /// `declare_item`'s `cranelift_linkage`). A within-process collision
    /// between two *different* strong symbols is the `@mangling(disabled)`/
    /// `@mangling(force = "...")` user error this check catches; it's
    /// untouched by generics, since the driver's own `ItemKey` cache
    /// already guarantees at most one `MirFunctionDef` per instantiation
    /// reaches this function within a single compilation -- weak linkage is
    /// only for folding two *separate* compilations' copies at link time.
    pub(super) fn declare_function_def(
        &mut self,
        function_def: &MirFunctionDef,
        symbol: String,
        linkage: Linkage,
    ) -> FuncId {
        let sig = self.function_signature(function_def);

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
            // Don't ask `cranelift_module` to declare a second `Export` for
            // a symbol it already has one for -- it rejects that outright
            // (a real, deliberate safety check on its part), which would
            // panic here instead of surfacing `symbol_error` cleanly. The
            // object file is discarded either way once `symbol_error` is
            // set (see `Codegen::generate`), so reusing the first
            // definition's `FuncId` for this one is harmless -- it only
            // has to survive long enough to let this pass finish.
            let existing_function_id = *self
                .functions
                .get(&existing_id)
                .expect("a declared symbol's owner is always already in `functions`");
            self.functions.insert(function_def.id, existing_function_id);
            return existing_function_id;
        }
        self.declared_symbols
            .insert(symbol.clone(), function_def.id);

        let function_id = self
            .module
            .declare_function(&symbol, linkage, &sig)
            .unwrap();

        self.functions.insert(function_def.id, function_id);
        function_id
    }

    /// `declare_function_def`'s extern-module counterpart: `Linkage::Import`
    /// only, no paired `Export`, and no body -- `define_item`'s pass 2
    /// never sees this `HirId` (it isn't in any `MirModule.items`). Trusts
    /// that the *other* `omgc` invocation compiling that module standalone
    /// mangled its own definition identically -- see
    /// `CompiledProgram::extern_functions`'s doc comment for why that's safe.
    pub(super) fn declare_extern_function(&mut self, extern_fn: &ExternFunctionRef) {
        // Symbol decision lives in `omega_mir::mangle::extern_function_ref_symbol`
        // -- one home shared by both backends.
        let mangled = omega_mir::mangle::extern_function_ref_symbol(extern_fn);
        let sig = self.make_function_sig(extern_fn.fn_type.clone());

        let function_id = self
            .module
            .declare_function(&mangled, Linkage::Import, &sig)
            .unwrap();
        self.functions.insert(extern_fn.decl_id, function_id);
    }

    /// Builds a function/method's body -- everything `update_function_def`
    /// used to do after declaring, now looking up the `FuncId` every item
    /// across every module already got in the declare pass, rather than
    /// declaring (and re-registering) it itself.
    ///
    /// There is no `BlockOutcome`/`return_block`/`loop_stack`/`defer_flags`
    /// bookkeeping here at all -- the mir body already *is* the
    /// control-flow graph (every `if`/`match`/`while`/`for`/`break`/
    /// `continue`/`return`/`defer` was flattened into it during lowering,
    /// see `docs/16-mir-and-codegen.md`), so this is just: one
    /// Cranelift `Block` per `MirBlockData`, then translate each one's
    /// statements and its single terminator.
    pub(super) fn define_function_def(&mut self, function_def: MirFunctionDef) {
        // A symbol collision (see `declare_function_def`) is always found
        // during the declare pass, which fully finishes before any define
        // pass starts -- so once one's been found, every remaining body is
        // skipped rather than defined against the improvised `FuncId` the
        // colliding function got (which would ask `cranelift_module` to
        // define the same `FuncId` twice and panic). `Codegen::generate`
        // discards this whole `Codegen` once `symbol_error` is `Some`.
        if self.symbol_error.is_some() {
            return;
        }

        let function_id = *self
            .functions
            .get(&function_def.id)
            .expect("declared for every item, across every module, before any body is defined");
        let sig = self.function_signature(&function_def);
        let MirFunctionDef {
            return_type, body, ..
        } = function_def;

        // Move `ctx` out of `self` for the duration of the build so the rest of
        // this function can still freely borrow `self` (e.g. `.cranelift_leaves(&self)`,
        // `self.process_expr(...)`) while `builder` holds onto it.
        let mut ctx = std::mem::replace(&mut self.ctx, codegen::Context::new());
        // `ctx.clear()` (called for every function via `clear_local`, below)
        // resets `want_disasm` back to `false` each time, so this has to be
        // set again per function, not once up front.
        ctx.set_disasm(self.emit == crate::EmitKind::Asm);
        let mut fbctx = FunctionBuilderContext::new();
        let mut builder = FunctionBuilder::new(&mut ctx.func, &mut fbctx);
        builder.func.signature = sig;

        self.arg_count = body.arg_count;
        self.local_args = vec![Vec::new(); body.locals.len()];

        // One combined stack slot for every non-parameter local, laid out
        // by `layout::locals_layout` -- the same field-layout algorithm a
        // struct's own fields already go through (see `frame_slot`'s own
        // doc comment for why this, not a per-local slot, is what makes a
        // zero-sized local's address genuinely coincide with whatever real
        // local comes next). Created unconditionally, even when
        // `packed_end` is `0` (no non-parameter locals, or all of them are
        // zero-sized) -- a zero-size stack slot is already relied on
        // elsewhere (an all-zero-leaf struct/`marker` local) and costs
        // nothing to create.
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

        // One cranelift block per mir block, minted up front so a forward
        // reference (or a loop's own back-edge) always resolves regardless
        // of which order the blocks below get filled in.
        let cranelift_blocks: Vec<cranelift::prelude::Block> =
            body.blocks.iter().map(|_| builder.create_block()).collect();
        let entry_block = cranelift_blocks[0];

        builder.append_block_params_for_function_params(entry_block);
        let block_params = builder.block_params(entry_block).to_vec();

        // A large return value comes back through a hidden StructReturn
        // pointer, always the signature's first parameter (see
        // `make_function_sig`) -- peel it off before mapping the *declared*
        // parameters below.
        let sret = self.needs_sret(&return_type).then(|| block_params[0]);
        let declared_params = &block_params[sret.is_some() as usize..];

        // A parameter can flatten to more than one leaf (e.g. a struct), so
        // repeat its local index once per leaf to map Cranelift's flat
        // param list back onto `local_args`.
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

        for (mir_block, &cranelift_block) in body.blocks.into_iter().zip(&cranelift_blocks) {
            builder.switch_to_block(cranelift_block);
            for stmt in mir_block.statements {
                self.process_expr(&mut builder, stmt);
            }
            self.emit_terminator(&mut builder, mir_block.terminator, &cranelift_blocks, sret);
        }

        // Every cranelift block was created up front and every terminator
        // above has now been emitted, so every block's predecessor set is
        // already final -- safe to seal all of them in one pass here,
        // rather than interleaved with the loop above.
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

        // `ctx.func` (the CLIF) and `ctx.compiled_code()` (populated by the
        // `define_function` call just above, since `set_disasm` was set
        // on this same `ctx` above) are both still valid to read here --
        // `define_function` fills in the compile *result*, it doesn't
        // consume the IR that produced it.
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

    /// Translates one `MirBlockData`'s single terminator into the
    /// Cranelift instruction(s) that end its corresponding block --
    /// `sret`, when `Some`, is the hidden struct-return pointer every
    /// `Return` in this function stores its value through instead of
    /// returning it in registers (see `make_function_sig`/`needs_sret`).
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
                builder.ins().jump(cranelift_blocks[target.0 as usize], &[]);
            }
            MirTerminator::Branch {
                condition,
                then_block,
                else_block,
            } => {
                let cond_value = self.process_expr(builder, condition)[0];
                builder.ins().brif(
                    cond_value,
                    cranelift_blocks[then_block.0 as usize],
                    &[],
                    cranelift_blocks[else_block.0 as usize],
                    &[],
                );
            }
            MirTerminator::Return(value) => {
                let leaves = value
                    .map(|v| self.process_expr(builder, v))
                    .unwrap_or_default();
                // With a StructReturn pointer, the value leaves are stored
                // through it and the signature declares no return values
                // (cranelift itself returns the pointer in rax per the
                // SysV rule); otherwise the leaves return in registers as
                // before.
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
