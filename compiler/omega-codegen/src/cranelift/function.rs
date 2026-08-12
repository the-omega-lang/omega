//! Building a Cranelift `Signature` for a function/method (shared by
//! definitions, externs, and call sites), and declaring/defining a
//! function's own symbol.

use super::Codegen;
use super::leaf::IntoCraneliftLeaves;
use crate::mangle;
use cranelift::codegen;
use cranelift::codegen::ir::ArgumentPurpose;
use cranelift::prelude::{
    AbiParam, FunctionBuilder, FunctionBuilderContext, Signature, StackSlotData, StackSlotKind, isa,
};
use cranelift_module::{FuncId, Linkage, Module};
use omega_analyzer::annotations::ManglingMode;
use omega_analyzer::checked::{ExternFunctionKind, ExternFunctionRef};
use omega_analyzer::layout;
use omega_analyzer::resolved_type::{ResolvedFunctionType, ResolvedType};
use omega_mir::{MirExternDeclaration, MirFunctionDef, MirTerminator};

impl Codegen {
    /// Whether `return_type` is returned through a hidden `StructReturn`
    /// pointer instead of in registers: x86_64 SysV has exactly two integer
    /// return registers (rax/rdx), so any value flattening to more than two
    /// leaves -- a large struct, or any enum with a payload -- can't come
    /// back by value. (Two int + two float leaves would technically still
    /// fit, but classifying leaf register classes buys nothing over this
    /// simple, always-correct rule.) Must agree between definitions and
    /// call sites -- both derive their `Signature` from `make_function_sig`,
    /// so it's decided in exactly one place.
    pub(super) fn needs_sret(&self, return_type: &ResolvedType) -> bool {
        return_type.cranelift_leaves(self).len() > 2
    }

    pub(super) fn make_function_sig(&self, resolved_fntype: ResolvedFunctionType) -> Signature {
        let mut sig = self.module.make_signature();

        // The hidden struct-return pointer is always the first parameter
        // (see `needs_sret`); cranelift itself handles the SysV requirement
        // of also returning that pointer in rax, so the signature declares
        // no return values at all in this case. `Never` takes this same
        // empty-signature path as `Void` -- nothing ever reads a call's
        // result in a position typed `never` (the callee doesn't return at
        // all, so there's no return-value ABI to negotiate); explicit here
        // rather than left to fall out of `cranelift_leaves`/`needs_sret`
        // both already answering "zero" for it (see `ResolvedType::Never`'s
        // doc comment) purely as a byproduct of how they're implemented.
        if *resolved_fntype.return_type != ResolvedType::Void
            && *resolved_fntype.return_type != ResolvedType::Never
        {
            if self.needs_sret(&resolved_fntype.return_type) {
                sig.params.push(AbiParam::special(
                    self.pointer_type(),
                    ArgumentPurpose::StructReturn,
                ));
            } else {
                for leaf in resolved_fntype.return_type.cranelift_leaves(self) {
                    sig.returns.push(AbiParam::new(leaf));
                }
            }
        }

        let ir_params = resolved_fntype
            .params
            .iter()
            .flat_map(|(_, ty)| ty.cranelift_leaves(self));
        for param in ir_params {
            sig.params.push(AbiParam::new(param));
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
                // `Disabled` is every ordinary, hand-written `extern`
                // (annotations are rejected on `extern` at parse time, so
                // this is the only reachable case for one of those) --
                // `Glued` is a `gap` declaration's synthesized required
                // function (see `CheckedExternDeclaration::mangling`'s doc
                // comment), which must link against the exact same symbol
                // its `glue` implementation forces -- computed the
                // identical way, via `mangle::glued_symbol`.
                let symbol = match &extern_decl.mangling {
                    ManglingMode::Disabled => extern_decl.ident.0.clone(),
                    ManglingMode::Glued {
                        spec_module_path,
                        spec_name,
                        function_name,
                    } => mangle::glued_symbol(
                        spec_module_path,
                        spec_name,
                        function_name,
                        &resolved_fntype,
                    ),
                    ManglingMode::Enabled | ManglingMode::Forced(_) => {
                        unreachable!(
                            "'@mangling' is rejected on 'extern' declarations at parse time"
                        )
                    }
                };
                let sig = self.make_function_sig(resolved_fntype);

                let function_id = self
                    .module
                    .declare_function(&symbol, Linkage::Import, &sig)
                    .unwrap();

                self.functions.insert(extern_decl.id, function_id);
            }

            _ => todo!("extern data declarations (non-function externs) are not yet implemented"),
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
    /// `Linkage::Preemptible` (weak) for a generic instantiation -- see
    /// `declare_item`'s `linkage_for`, this function's only caller for the
    /// choice. A within-process collision between two *different* strong
    /// symbols is still exactly the `@mangling(disabled)`/`@mangling(force =
    /// "...")` user error this check has always caught; it's untouched by
    /// generics, since the
    /// driver's own `ItemKey` cache already guarantees at most one
    /// `MirFunctionDef` per instantiation reaches this function at all
    /// within a single compilation -- weak linkage is what lets two
    /// *separate* compilations' independently-generated copies fold into
    /// one at link time, a scenario this in-process map never sees.
    ///
    /// A single `declare_function` call with the real `linkage` already
    /// covers both "first time this symbol is seen" and "seen again, merge
    /// linkages" (`cranelift_module`'s own `Linkage::merge` treats
    /// `Import` as identity, so pre-declaring as `Import` and immediately
    /// re-declaring with the real linkage -- what this used to do -- is
    /// provably the same as just declaring with `linkage` directly).
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

    /// `declare_function_def`'s extern-module counterpart: declares a link
    /// against an extern-owned function/method, but `Linkage::Import` only
    /// -- no paired `Export` declare, and `define_item`'s pass 2 never sees
    /// this `HirId` at all (it isn't in any `MirModule.items`), so no body
    /// is ever generated for it here. `extern_fn.mangling` (resolved by the
    /// *declaring* compilation, at signature time -- see
    /// `omega_analyzer::annotations`' doc comment and `ExternFunctionRef::
    /// mangling`'s own) decides which symbol-shape branch below applies,
    /// mirroring `declare_item`'s identical branch for a local function:
    /// whatever that other `omgc` invocation actually mangled this
    /// declaration as is exactly what gets linked against here, never
    /// assumed. Trusts that the *other* `omgc` invocation compiling that
    /// module standalone mangles its own definition identically -- see
    /// `CompiledProgram::extern_functions`'s doc comment for why that's a
    /// safe assumption.
    pub(super) fn declare_extern_function(&mut self, extern_fn: &ExternFunctionRef) {
        let mangled = match (&extern_fn.mangling, &extern_fn.kind) {
            (ManglingMode::Forced(name), _) => name.clone(),
            (
                ManglingMode::Glued {
                    spec_module_path,
                    spec_name,
                    function_name,
                },
                _,
            ) => mangle::glued_symbol(
                spec_module_path,
                spec_name,
                function_name,
                &extern_fn.fn_type,
            ),
            (ManglingMode::Disabled, ExternFunctionKind::Free(name)) => name.as_ref().to_string(),
            // `@mangling(disabled)` is rejected on methods at analysis time
            // -- an extern method's own declaration went through the exact
            // same check, so this combination can't actually occur.
            (
                ManglingMode::Disabled,
                ExternFunctionKind::Method { .. }
                | ExternFunctionKind::Primitive { .. }
                | ExternFunctionKind::Compose { .. },
            ) => {
                unreachable!("'@mangling(disabled)' is rejected on methods at analysis time")
            }
            // `collect_extern_functions` only ever surfaces non-generic
            // extern items (a generic reached through `--extern` is always
            // fully recompiled locally instead), so there's no owner/free
            // generic-args data to pass here -- always `&[]`.
            (ManglingMode::Enabled, ExternFunctionKind::Free(name)) => {
                mangle::encode(&mangle::free_function_symbol(
                    &extern_fn.module_path,
                    name,
                    &[],
                    &extern_fn.fn_type,
                ))
            }
            (
                ManglingMode::Enabled,
                ExternFunctionKind::Method {
                    type_name,
                    method_name,
                },
            ) => mangle::encode(&mangle::method_symbol(
                &extern_fn.module_path,
                type_name,
                &[],
                method_name,
                &extern_fn.fn_type,
            )),
            (
                ManglingMode::Enabled,
                ExternFunctionKind::Primitive {
                    target,
                    method_name,
                },
            ) => {
                mangle::encode(&mangle::primitive_method_symbol(
                    target,
                    method_name,
                    &extern_fn.fn_type,
                ))
            }
            (
                ManglingMode::Enabled,
                ExternFunctionKind::Compose {
                    target,
                    spec_name,
                    spec_args,
                    method_name,
                },
            ) => mangle::encode(&mangle::compose_method_symbol(
                target,
                spec_name,
                spec_args,
                method_name,
                &extern_fn.fn_type,
            )),
        };
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
        // pass starts (see `update_all`'s doc comment) -- so once one's
        // been found, every remaining body is skipped outright rather than
        // defined against whatever `FuncId` `declare_function_def` had to
        // improvise for the colliding function (which would otherwise ask
        // `cranelift_module` to define the same `FuncId` twice and panic).
        // `Codegen::generate` discards this whole `Codegen` once
        // `symbol_error` is `Some`, so an incomplete module here is fine.
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

        // Some identifiers (e.g structs) have more than one value per identifier.
        // For that reason, lets make a helper array that repeats the local's own
        // index N times, where N is the amount of values it has.
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
