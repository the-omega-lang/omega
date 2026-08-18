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
use omega_analyzer::{Arch, Os, Target};
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
    /// The compilation target in Omega's own vocabulary -- what the shared
    /// ABI facts (`crate::abi::AbiSignature`) are built against.
    target: Target,
    module: ObjectModule,
    functions: HashMap<HirId, FuncId>,
    ctx: codegen::Context,
    emit: EmitKind,
    /// Accumulated by `define_function_def` when `emit` is `Ir`/`Asm` --
    /// empty (and never appended to) for `Obj`, so the common case pays no
    /// cost for a feature it isn't using.
    captured_text: String,

    // Global state
    /// Every anonymous byte-run constant emitted so far -- `"..."` (`*str`)
    /// and `b"..."` (`*[u8]`) literals share one map, deduplicated by raw
    /// content: neither is null-terminated, so identical text used either
    /// way produces byte-for-byte identical storage, and the two only ever
    /// differ in the surrounding expression's *type*.
    bytes: HashMap<String, DataId>,
    /// `build_const_data`/`build_const_slice_data`'s dedup cache, keyed by
    /// their content-addressed symbol name -- the `comp`/const-slice
    /// counterpart of `bytes` above, kept separate since the two are keyed
    /// by different things (raw string content vs. an arbitrary
    /// `ConstValue`'s hash). Without it, two independent constant
    /// expressions evaluating to identical content would both try to
    /// `define_data` under the same symbol, which `cranelift_module`
    /// rejects.
    const_blobs: HashMap<String, DataId>,
    /// One vtable per distinct resolved slot list (`MirSpecCoerce::slots`),
    /// built lazily on first use (see `vtable::vtable_for`) and shared by
    /// every later coercion with the same slot list. Keyed on `slots`
    /// itself rather than `(concrete, spec)` -- see `vtable_for`'s own doc
    /// comment for why identity isn't precise enough once one implementor
    /// can satisfy the same generic spec twice.
    vtables: HashMap<Vec<HirId>, DataId>,
    /// Every top-level global's `DataId`, keyed by its declaration id --
    /// filled in by `declare_item`'s `Declaration` arm (pass 1), read back
    /// by `Storage::Global` place codegen whenever a reference needs its
    /// address.
    globals: HashMap<HirId, DataId>,

    // Local state (must be cleared per function)
    /// One entry per `MirBody::locals` index -- `local_args[i]` is
    /// non-empty exactly when local `i` is a parameter (`i < arg_count`),
    /// caching its already-materialized SSA leaves straight from the entry
    /// block's own Cranelift params. Spilled to a stack slot on demand if
    /// something later takes its address -- see `place::place_storage_address`.
    local_args: Vec<Vec<Value>>,
    /// One combined stack slot for *every* non-parameter local in the
    /// current function (`i >= arg_count`), sized to
    /// `layout::locals_layout`'s own `packed_end` and created once, up
    /// front, in `define_function_def` -- never one slot per local, so a
    /// zero-sized local's offset still coincides with whatever real local
    /// comes next, the same way a zero-sized struct field does. `None`
    /// only between functions (`clear_local`); always `Some` once any
    /// block in the current function is processed.
    frame_slot: Option<StackSlot>,
    /// `MirBody::locals`-indexed (full length, matching `local_args`) --
    /// `local_offsets[i]` (`i >= arg_count`) is `i`'s own byte offset into
    /// `frame_slot`, precomputed once per function by `layout::
    /// locals_layout` alongside `frame_slot` itself. Entries `< arg_count`
    /// are never read (a parameter's storage is `local_args`, not
    /// `frame_slot`).
    local_offsets: Vec<u32>,
    /// The current function's own `MirBody::arg_count` -- the boundary
    /// `local_args`/`frame_slot` use to tell a parameter local apart from
    /// a declared/synthetic one.
    arg_count: usize,

    /// Every locally-defined function/method's final linker symbol, keyed
    /// by the string handed to `cranelift_module::declare_function` --
    /// built up as `declare_function_def` runs for every item (see
    /// `update_all`). A second, different function claiming a symbol
    /// already seen (only possible via `@mangling(disabled)` or
    /// `@mangling(force = "...")`) is caught here instead of surfacing as a
    /// confusing linker error or a silent single-definition merge.
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
        let CodegenRequest { module_name, target, opt_level, emit, modules, entry: _, extern_functions } = request;

        let isa = {
            let mut builder = settings::builder();

            builder.set("opt_level", cranelift_opt_setting(opt_level)).unwrap();
            builder.enable("is_pic").unwrap();

            let flags = settings::Flags::new(builder);

            isa::lookup(triple_for(target))
                .map_err(|e| format!("target '{target}' is not supported by this build of the compiler: {e}"))?
                .finish(flags)
                .map_err(|e| format!("failed to build a code generator for target '{target}': {e}"))?
        };

        let module = {
            let translation_unit_name = module_name.bytes().collect::<Vec<_>>();
            let libcall_names = cranelift_module::default_libcall_names();
            let mut builder = ObjectBuilder::new(isa.clone(), translation_unit_name, libcall_names).unwrap();
            builder.per_function_section(true);
            ObjectModule::new(builder)
        };

        let mut codegen = Self {
            isa,
            target,
            module,
            functions: HashMap::new(),
            ctx: codegen::Context::new(),
            emit,
            captured_text: String::new(),

            bytes: HashMap::new(),
            const_blobs: HashMap::new(),
            vtables: HashMap::new(),
            globals: HashMap::new(),

            local_args: Vec::new(),
            frame_slot: None,
            local_offsets: Vec::new(),
            arg_count: 0,
            declared_symbols: HashMap::new(),
            symbol_error: None,
        };

        codegen.update_all(modules, extern_functions);

        if let Some(error) = codegen.symbol_error {
            return Err(error);
        }

        Ok(codegen)
    }

    fn clear_local(&mut self) {
        self.ctx.clear();
        self.frame_slot = None;
        self.local_offsets.clear();
        self.local_args.clear();
        self.arg_count = 0;
    }

    pub(super) fn pointer_type(&self) -> IRType {
        self.module.target_config().pointer_type()
    }

    /// The width of a pointer on the target this `Codegen` was built for,
    /// in bytes -- what every `omega_analyzer::layout` call site needs in place of
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

/// The targets this build of the Cranelift backend can serve -- its
/// `cranelift-codegen` dependency requests the `x86` and `arm64` ISAs, so
/// those two arches are genuinely buildable; everything else in
/// `Target::Arch` (the 32-bit arches, riscv) is *not* compiled into
/// Cranelift here, and `isa::lookup` would fail on them. Kept as the
/// backend's own answer to `BackendKind::supports`.
pub(crate) fn supports(target: Target) -> bool {
    matches!(target.arch, Arch::X86_64 | Arch::Aarch64)
}

/// This backend's entry point, called from `crate::generate`'s dispatch --
/// see `crate::BackendKind`.
pub(crate) fn generate(request: CodegenRequest) -> Result<EmitOutput, String> {
    Ok(Codegen::generate(request)?.finish())
}

/// The Cranelift-specific translation of an Omega [`Target`] -- private to
/// this backend: nothing outside the `cranelift` module should ever need a
/// `target_lexicon::Triple`. Each OS gets the vendor/environment/
/// binary-format combination its own platform actually uses (e.g. ELF
/// + GNU on Linux, Mach-O + Apple on macOS); the user-facing `Target`
/// stays deliberately simpler than Cranelift's own 5-field `Triple`
/// because Omega has no use for those extra axes today.
fn triple_for(target: Target) -> target_lexicon::Triple {
    use target_lexicon::{Architecture, Environment, OperatingSystem, Triple, Vendor};
    let architecture = match target.arch {
        Arch::X86_64 => Architecture::X86_64,
        Arch::X86 => Architecture::X86_32(target_lexicon::X86_32Architecture::I686),
        Arch::Armv7 => Architecture::Arm(target_lexicon::ArmArchitecture::Armv7),
        Arch::Thumbv7em => {
            Architecture::Arm(target_lexicon::ArmArchitecture::Thumbv7em)
        }
        Arch::Aarch64 => Architecture::Aarch64(target_lexicon::Aarch64Architecture::Aarch64),
        Arch::Riscv32 => Architecture::Riscv32(target_lexicon::Riscv32Architecture::Riscv32),
        Arch::Riscv64 => Architecture::Riscv64(target_lexicon::Riscv64Architecture::Riscv64),
    };
    let (vendor, operating_system, environment, binary_format) = match target.os {
        Os::None => (
            Vendor::Unknown,
            OperatingSystem::Unknown,
            Environment::Unknown,
            target_lexicon::BinaryFormat::Elf,
        ),
        Os::Linux => {
            (Vendor::Unknown, OperatingSystem::Linux, Environment::Gnu, target_lexicon::BinaryFormat::Elf)
        }
        Os::MacOs => (
            Vendor::Apple,
            OperatingSystem::MacOSX(None),
            Environment::Unknown,
            target_lexicon::BinaryFormat::Macho,
        ),
        Os::Windows => {
            (Vendor::Pc, OperatingSystem::Windows, Environment::Msvc, target_lexicon::BinaryFormat::Coff)
        }
    };
    Triple { architecture, vendor, operating_system, environment, binary_format }
}
