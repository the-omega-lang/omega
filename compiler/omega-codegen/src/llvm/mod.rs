//! The LLVM backend -- the second implementation behind `crate::BackendKind`,
//! at capability parity with `cranelift`. Built against the corrected seam:
//! the MIR carries every decided fact (symbol, linkage, access alignment),
//! the shared ABI (`crate::abi`) carries the calling convention, and the
//! shared preflight (`crate::preflight`) carries the rejections -- so this
//! backend's only real work is *translating* those facts into LLVM.
//!
//! Like its Cranelift sibling, `Codegen` never fails on the *program*
//! itself: everything rejectable was already enforced by analysis or by
//! `crate::preflight`. The only fallible steps here are target-machine
//! construction (a `--target` this LLVM build cannot serve) and the
//! shared symbol-collision check -- both plain `Err(String)`, matching
//! `omgc`'s CLI-error convention.

mod expr;
mod function;
mod item;
mod leaf;
mod place;
mod vtable;

use crate::{CodegenRequest, EmitKind, EmitOutput, OptLevel};
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::targets::{FileType, InitializationConfig, RelocMode, Target, TargetMachine, TargetTriple};
use inkwell::OptimizationLevel;
use omega_analyzer::{Arch, Os, Target as OmegaTarget};
use omega_hir::HirId;
use std::collections::HashMap;

/// The targets this LLVM build can serve -- every `Target` arch, since
/// LLVM ships them all in this build (`Target::initialize_all`). Kept as
/// the backend's own answer to `BackendKind::supports`.
pub(crate) fn supports(_target: OmegaTarget) -> bool {
    true
}

/// The LLVM triple for an Omega `Target` -- LLVM's own vocabulary, private
/// to this backend exactly like `cranelift::triple_for` is to Cranelift's.
fn triple_for(target: OmegaTarget) -> String {
    let arch = match target.arch {
        Arch::X86_64 => "x86_64",
        Arch::X86 => "i686",
        Arch::Armv7 => "armv7",
        Arch::Thumbv7em => "thumbv7em",
        Arch::Aarch64 => "aarch64",
        Arch::Riscv32 => "riscv32",
        Arch::Riscv64 => "riscv64",
    };
    match target.os {
        Os::None => format!("{arch}-unknown-none-elf"),
        Os::Linux => format!("{arch}-unknown-linux-gnu"),
        Os::MacOs => format!("{arch}-apple-macosx"),
        Os::Windows => format!("{arch}-pc-windows-msvc"),
    }
}

/// `-O<n>` maps onto LLVM's own four levels natively -- unlike Cranelift,
/// LLVM *has* four, so nothing collapses and nothing is invented; the
/// difference is documented (`docs/16-mir-and-codegen.md`) rather than
/// papered over.
fn llvm_opt_level(level: OptLevel) -> OptimizationLevel {
    match level {
        OptLevel::O0 => OptimizationLevel::None,
        OptLevel::O1 => OptimizationLevel::Less,
        OptLevel::O2 => OptimizationLevel::Default,
        OptLevel::O3 => OptimizationLevel::Aggressive,
    }
}

pub(crate) struct Codegen<'ctx> {
    context: &'ctx Context,
    module: Module<'ctx>,
    builder: Builder<'ctx>,
    target_machine: TargetMachine,
    target: OmegaTarget,
    emit: EmitKind,

    /// Every locally-defined function/method/extern's own `FunctionValue`,
    /// keyed by declaration id -- the `functions` counterpart of
    /// `cranelift::Codegen::functions`.
    functions: HashMap<HirId, inkwell::values::FunctionValue<'ctx>>,
    /// Every anonymous byte-run constant this module has emitted so far,
    /// deduplicated by raw content -- the exact counterpart of
    /// `cranelift::Codegen::bytes`.
    bytes: HashMap<String, inkwell::values::GlobalValue<'ctx>>,
    /// The content-addressed `comp`/const-slice dedup cache -- the exact
    /// counterpart of `cranelift::Codegen::const_blobs`.
    const_blobs: HashMap<String, inkwell::values::GlobalValue<'ctx>>,
    /// One vtable per distinct resolved slot list -- the exact counterpart
    /// of `cranelift::Codegen::vtables`.
    vtables: HashMap<Vec<HirId>, inkwell::values::GlobalValue<'ctx>>,
    /// Every top-level global's own `GlobalValue`, keyed by declaration id.
    globals: HashMap<HirId, inkwell::values::GlobalValue<'ctx>>,

    /// The declared-symbol collision guard -- the exact counterpart of
    /// `cranelift::Codegen::declared_symbols`/`symbol_error`.
    declared_symbols: HashMap<String, HirId>,
    symbol_error: Option<String>,

    // Local state (cleared per function)
    local_args: Vec<Vec<inkwell::values::BasicValueEnum<'ctx>>>,
    frame_slot: Option<inkwell::values::PointerValue<'ctx>>,
    /// The function currently being defined's own entry block -- where
    /// *every* `alloca` this backend emits goes, whichever block asked for
    /// it. See `entry_alloca`.
    entry_block: Option<inkwell::basic_block::BasicBlock<'ctx>>,
    local_offsets: Vec<u32>,
    arg_count: usize,
}

impl<'ctx> Codegen<'ctx> {
    /// Builds the target machine from `request.target`/`request.opt_level`
    /// and runs the whole declare-then-define pipeline.
    fn generate(
        context: &'ctx Context,
        request: CodegenRequest,
    ) -> Result<Self, String> {
        let CodegenRequest { module_name, target, opt_level, emit, modules, entry: _, extern_functions } =
            request;

        // Every target this LLVM build ships is registered up front -- the
        // 32-bit/freestanding targets Phase A widened `Target` for are
        // only buildable through this backend.
        Target::initialize_all(&InitializationConfig::default());

        let triple = TargetTriple::create(&triple_for(target));
        let llvm_target = Target::from_triple(&triple)
            .map_err(|e| format!("target '{target}' is not supported by this build of the compiler: {e}"))?;
        let target_machine = llvm_target
            .create_target_machine(
                &triple,
                "generic",
                "",
                llvm_opt_level(opt_level),
                // PIC, like Cranelift's `is_pic` -- both required here or
                // the two backends' objects stop being interchangeable.
                RelocMode::PIC,
                inkwell::targets::CodeModel::Default,
            )
            .ok_or_else(|| format!("failed to build a code generator for target '{target}'"))?;

        let module = context.create_module(&module_name);
        module.set_triple(&target_machine.get_triple());
        module.set_data_layout(&target_machine.get_target_data().get_data_layout());

        let mut codegen = Self {
            context,
            module,
            builder: context.create_builder(),
            target_machine,
            target,
            emit,
            functions: HashMap::new(),
            bytes: HashMap::new(),
            const_blobs: HashMap::new(),
            vtables: HashMap::new(),
            globals: HashMap::new(),
            declared_symbols: HashMap::new(),
            symbol_error: None,
            local_args: Vec::new(),
            frame_slot: None,
            entry_block: None,
            local_offsets: Vec::new(),
            arg_count: 0,
        };

        codegen.update_all(modules, extern_functions);

        if let Some(error) = codegen.symbol_error {
            return Err(error);
        }

        // LLVM will happily write malformed IR straight into an object file
        // -- a type-mismatched `icmp`, a call with the wrong arity -- and
        // the result crashes at *run* time, in generated code, with nothing
        // pointing back at the compiler. The verifier turns that class of
        // backend bug into a compile-time failure instead, which is the only
        // way it is ever debuggable. Cranelift needs no counterpart: its
        // builder validates as it goes and panics at the point of
        // construction.
        //
        // This is a bug in *this compiler*, never in the program being
        // compiled: every rejectable program input was settled long before
        // codegen (see `crate::preflight`). It is surfaced rather than
        // `panic!`ed only so the message survives to the user intact.
        if let Err(errors) = codegen.module.verify() {
            return Err(format!(
                "internal error: the LLVM backend produced invalid IR -- this is a compiler bug, \
                 not a problem with your program.\n{}",
                errors.to_string().trim_end()
            ));
        }

        Ok(codegen)
    }

    fn clear_local(&mut self) {
        self.frame_slot = None;
        self.entry_block = None;
        self.local_offsets.clear();
        self.local_args.clear();
        self.arg_count = 0;
    }

    /// A `bytes`-sized, `align`-byte-aligned scratch slot, allocated in the
    /// function's **entry block** no matter which block asked for it.
    ///
    /// This is the one place this backend allocates stack storage, and the
    /// entry block is not a stylistic preference: LLVM's `alloca` allocates
    /// *each time it executes*, and the stack is only released at `ret`, so
    /// an `alloca` emitted at the current insertion point inside a loop body
    /// grows the frame once per iteration and eventually overflows it (an
    /// enum constructed in a `loop` is enough). Cranelift's
    /// `create_sized_stack_slot` has no such hazard -- a stack slot is a
    /// property of the *function*, allocated once in the prologue -- so
    /// hoisting to the entry block is exactly what makes the two backends
    /// agree. Reuse across iterations is safe for the same reason it is in
    /// Cranelift: every one of these slots is fully written before it is
    /// read, at every site that asks for one.
    ///
    /// The builder's own insertion point is saved and restored, so a caller
    /// mid-way through emitting some other block is unaffected.
    pub(super) fn entry_alloca(
        &self,
        bytes: u32,
        align: u32,
        name: &str,
    ) -> inkwell::values::PointerValue<'ctx> {
        let entry = self
            .entry_block
            .expect("define_function_def sets this before any block is emitted");
        let resume = self.builder.get_insert_block();
        // Before the entry block's first instruction, not at its end: by the
        // time a later block asks for a slot, the entry block already ends
        // in a terminator, and nothing may follow that.
        match entry.get_first_instruction() {
            Some(first) => self.builder.position_before(&first),
            None => self.builder.position_at_end(entry),
        }
        let byte_array = self.context.i8_type().array_type(bytes.max(1));
        let slot = self.builder.build_alloca(byte_array, name).expect("alloca always succeeds");
        if let Some(inst) = slot.as_instruction() {
            let _ = inst.set_alignment(align);
        }
        if let Some(block) = resume {
            self.builder.position_at_end(block);
        }
        slot
    }

    /// The universal pointer type for this compilation -- see
    /// `leaf::ptr_type`'s doc comment.
    pub(super) fn ptr_type(&self) -> inkwell::types::PointerType<'ctx> {
        leaf::ptr_type(self.context)
    }

    /// The width of a pointer on the target this `Codegen` was built for,
    /// in bytes -- the shared layout math's parameter, straight from the
    /// target (see `Target::pointer_bytes`).
    pub(super) fn pointer_bytes(&self) -> u32 {
        self.target.pointer_bytes()
    }

    /// Produces whatever `emit` asked for. `Obj` finishes and links the
    /// object; `Ir`/`Asm` print the module's own textual forms instead.
    fn finish(self) -> EmitOutput {
        match self.emit {
            EmitKind::Obj => {
                let buffer = self
                    .target_machine
                    .write_to_memory_buffer(&self.module, FileType::Object)
                    .expect("object emission cannot fail for a validated target machine");
                EmitOutput::Object(buffer.as_slice().to_vec())
            }
            EmitKind::Ir => EmitOutput::Text(self.module.print_to_string().to_string()),
            EmitKind::Asm => {
                let buffer = self
                    .target_machine
                    .write_to_memory_buffer(&self.module, FileType::Assembly)
                    .expect("assembly emission cannot fail for a validated target machine");
                EmitOutput::Text(String::from_utf8_lossy(buffer.as_slice()).into_owned())
            }
        }
    }
}

/// This backend's entry point, called from `crate::generate`'s dispatch --
/// see `crate::BackendKind`.
pub(crate) fn generate(request: CodegenRequest) -> Result<EmitOutput, String> {
    let context = Context::create();
    Codegen::generate(&context, request).map(Codegen::finish)
}
