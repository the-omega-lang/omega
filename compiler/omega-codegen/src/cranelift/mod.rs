//! The Cranelift backend -- the only one implemented today (see
//! `crate::BackendKind`). `Codegen` never fails on the *program* itself:
//! everything it would otherwise need to reject was already enforced
//! while building the `CheckedModule` these `MirModule`s were lowered
//! from (place validity, type compatibility, field/index existence,
//! redeclaration). What remains here are cases the language genuinely
//! hasn't decided yet (array memory layout, global data storage, ...) --
//! those `panic!`/`todo!()` rather than returning an error, since there is
//! no rejectable *program* input left by the time codegen runs, only
//! unimplemented compiler features. The one exception is `generate`
//! itself: a `--target`/ISA construction failure is genuinely rejectable
//! *CLI* input (unlike anything about the program being compiled), so it
//! alone returns a `Result`.

mod expr;
mod function;
mod item;
mod leaf;
mod place;
mod vtable;

use crate::{CodegenRequest, EmitKind, EmitOutput, OptLevel};
use cranelift::codegen;
use cranelift::codegen::ir::StackSlot;
use cranelift::prelude::{Configurable, Type as IRType, Value, isa, settings};
use cranelift_module::{DataId, FuncId, Module};
use cranelift_object::{ObjectBuilder, ObjectModule};
use omega_hir::HirId;
use std::collections::HashMap;
use std::sync::Arc;

pub(crate) struct Codegen {
    // Backend
    isa: Arc<dyn isa::TargetIsa>,
    module: ObjectModule,
    functions: HashMap<HirId, FuncId>,
    ctx: codegen::Context,
    emit: EmitKind,
    /// Accumulated by `define_function_def` when `emit` is `Ir`/`Asm` --
    /// empty (and never appended to) for `Obj`, so the common case pays no
    /// cost for a feature it isn't using.
    captured_text: String,

    // Global state
    /// Every anonymous byte-run constant this module has emitted so far --
    /// `"..."` (`*str`) and `b"..."` (`*[u8]`) literals alike, deduplicated
    /// by raw content in one map: neither is null-terminated, so identical
    /// text used once each way produces byte-for-byte identical storage,
    /// and sharing one `DataId` between them is exactly right (they only
    /// ever differ in the *type* the surrounding expression carries, never
    /// in the bytes themselves).
    bytes: HashMap<String, DataId>,
    /// One vtable per distinct resolved slot list (`MirSpecCoerce::slots`)
    /// actually coerced to a `spec *Spec` value somewhere in this
    /// compilation -- built lazily, the first time a `MirExpr::SpecCoerce`
    /// with that exact slot list is codegen'd (see `vtable::vtable_for`),
    /// and shared by every later coercion with the same one. Keyed on
    /// `slots` itself rather than `(concrete, spec)`: two coercions
    /// resolving to the identical ordered method list always produce
    /// byte-identical vtables regardless of which concrete type or spec
    /// they came from, so `slots` alone is both simpler and strictly more
    /// precise than an identity-based key (see `vtable_for`'s own doc
    /// comment for why the old `(concrete, spec)` key stopped being enough
    /// once one implementor could satisfy the same generic spec twice).
    vtables: HashMap<Vec<HirId>, DataId>,

    // Local state (must be cleared per function)
    /// One entry per `MirBody::locals` index -- `local_args[i]` is
    /// non-empty exactly when local `i` is a parameter (`i < arg_count`),
    /// caching its already-materialized SSA leaves straight from the entry
    /// block's own Cranelift params (never a stack slot, unless something
    /// later takes its address -- see `place::place_storage_address`'s
    /// own `todo!()`).
    local_args: Vec<Vec<Value>>,
    /// `MirBody::locals`-indexed, one stack slot per *non-parameter* local
    /// (`i >= arg_count`), sized to its type's total byte size (not one
    /// slot per scalar leaf) -- a prerequisite for `&`/`*`: a local needs a
    /// single address, and three independent per-leaf slots have three
    /// unrelated addresses. Sized to `MirBody::locals.len()` up front (all
    /// `None`) but each entry is only actually allocated lazily, the first
    /// time `resolve_place_storage` resolves that local -- a branch that
    /// never runs never pays for a slot it never touches.
    stack_slots: Vec<Option<StackSlot>>,
    /// The current function's own `MirBody::arg_count` -- the boundary
    /// `local_args`/`stack_slots` use to tell a parameter local apart from
    /// a declared/synthetic one (see `MirBody::locals`'s own doc comment
    /// for why both share one index space).
    arg_count: usize,

    /// Every locally-defined function/method's final (post-mangling-or-not)
    /// linker symbol, keyed by the demangled string actually handed to
    /// `cranelift_module::declare_function` -- built up as
    /// `declare_function_def` runs for every item across every module (see
    /// `update_all`). A second, different function claiming a symbol
    /// already seen (only possible via `@mangling(disabled)`, since a
    /// mangled name always embeds a unique module path + `HirId`) is caught
    /// here instead of surfacing as a confusing linker error or, worse, a
    /// silent single-definition merge.
    declared_symbols: HashMap<String, HirId>,
    /// The first symbol collision found (see `declared_symbols`) -- checked
    /// once, at the end of `generate`, and turned into that function's
    /// `Err`.
    symbol_error: Option<String>,
}

/// `-O<n>` maps onto this. Cranelift's own `opt_level` setting only has
/// three values (`none`/`speed`/`speed_and_size`), one fewer than
/// `OptLevel`'s four, so `O1`/`O2` deliberately collapse onto the same
/// Cranelift setting rather than inventing a distinction Cranelift itself
/// doesn't make.
fn cranelift_opt_setting(level: OptLevel) -> &'static str {
    match level {
        OptLevel::O0 => "none",
        OptLevel::O1 | OptLevel::O2 => "speed",
        OptLevel::O3 => "speed_and_size",
    }
}

impl Codegen {
    /// Builds a `TargetIsa` from `request.target`/`request.opt_level` and
    /// runs the whole declare-then-define pipeline.
    fn generate(request: CodegenRequest) -> Result<Self, String> {
        let CodegenRequest { module_name, target, opt_level, emit, modules, entry, extern_functions } = request;

        let isa = {
            let mut builder = settings::builder();

            builder.set("opt_level", cranelift_opt_setting(opt_level)).unwrap();
            builder.enable("is_pic").unwrap();

            let flags = settings::Flags::new(builder);

            isa::lookup(target.to_triple())
                .map_err(|e| format!("target '{target}' is not supported by this build of the compiler: {e}"))?
                .finish(flags)
                .map_err(|e| format!("failed to build a code generator for target '{target}': {e}"))?
        };

        let module = {
            let translation_unit_name = module_name.bytes().collect::<Vec<_>>();
            let libcall_names = cranelift_module::default_libcall_names();
            let builder = ObjectBuilder::new(isa.clone(), translation_unit_name, libcall_names).unwrap();
            ObjectModule::new(builder)
        };

        let mut codegen = Self {
            isa,
            module,
            functions: HashMap::new(),
            ctx: codegen::Context::new(),
            emit,
            captured_text: String::new(),

            bytes: HashMap::new(),
            vtables: HashMap::new(),

            local_args: Vec::new(),
            stack_slots: Vec::new(),
            arg_count: 0,
            declared_symbols: HashMap::new(),
            symbol_error: None,
        };

        codegen.update_all(modules, &entry, extern_functions);

        if let Some(error) = codegen.symbol_error {
            return Err(error);
        }

        Ok(codegen)
    }

    fn clear_local(&mut self) {
        self.ctx.clear();
        self.stack_slots.clear();
        self.local_args.clear();
        self.arg_count = 0;
    }

    pub(super) fn pointer_type(&self) -> IRType {
        self.module.target_config().pointer_type()
    }

    /// The width of a pointer on the target this `Codegen` was built for,
    /// in bytes -- what every `crate::layout` call site needs in place of
    /// the `&Codegen` those functions used to take directly.
    pub(super) fn pointer_bytes(&self) -> u32 {
        self.pointer_type().bytes()
    }

    /// Produces whatever `emit` (passed to `generate`) asked for. `Obj`
    /// finishes and links the object; `Ir`/`Asm` skip linking entirely --
    /// nothing downstream needs the linked object in those modes, only the
    /// text `function::define_function_def` already accumulated into
    /// `captured_text` as each function compiled.
    fn finish(self) -> EmitOutput {
        match self.emit {
            EmitKind::Obj => EmitOutput::Object(self.module.finish().emit().unwrap()),
            EmitKind::Ir | EmitKind::Asm => EmitOutput::Text(self.captured_text),
        }
    }
}

/// This backend's entry point, called from `crate::generate`'s dispatch --
/// see `crate::BackendKind`.
pub(crate) fn generate(request: CodegenRequest) -> Result<EmitOutput, String> {
    Ok(Codegen::generate(request)?.finish())
}
