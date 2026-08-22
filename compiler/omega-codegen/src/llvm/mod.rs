mod constant;
mod expr;
mod function;
mod inline_asm;
mod item;
mod leaf;
mod place;
mod vtable;

use crate::symbol::SymbolRegistry;
use crate::{CodegenRequest, EmitKind, EmitOutput, OptLevel};
use inkwell::OptimizationLevel;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::targets::{
    FileType, InitializationConfig, RelocMode, Target, TargetMachine, TargetTriple,
};
use omega_analyzer::{Arch, Os, Target as OmegaTarget};
use omega_hir::HirId;
use std::collections::HashMap;

pub(crate) fn supports(_target: OmegaTarget) -> bool {
    true
}

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

    functions: HashMap<HirId, inkwell::values::FunctionValue<'ctx>>,
    bytes: HashMap<String, inkwell::values::GlobalValue<'ctx>>,
    const_blobs: HashMap<String, inkwell::values::GlobalValue<'ctx>>,
    vtables: HashMap<String, inkwell::values::GlobalValue<'ctx>>,
    globals: HashMap<HirId, inkwell::values::GlobalValue<'ctx>>,

    symbols: SymbolRegistry,

    local_args: Vec<Vec<inkwell::values::BasicValueEnum<'ctx>>>,
    parameter_slots: Vec<Option<inkwell::values::PointerValue<'ctx>>>,
    frame_slot: Option<inkwell::values::PointerValue<'ctx>>,
    entry_block: Option<inkwell::basic_block::BasicBlock<'ctx>>,
    local_offsets: Vec<u32>,
    arg_count: usize,
}

impl<'ctx> Codegen<'ctx> {
    fn generate(context: &'ctx Context, request: CodegenRequest) -> Result<Self, String> {
        let CodegenRequest {
            module_name,
            target,
            opt_level,
            emit,
            modules,
            entry: _,
            extern_functions,
        } = request;

        // Initialize all LLVM targets before creating the requested target machine.
        Target::initialize_all(&InitializationConfig::default());

        let triple = TargetTriple::create(&triple_for(target));
        let llvm_target = Target::from_triple(&triple).map_err(|e| {
            format!("target '{target}' is not supported by this build of the compiler: {e}")
        })?;
        let target_machine = llvm_target
            .create_target_machine(
                &triple,
                "generic",
                "",
                llvm_opt_level(opt_level),
                // Use PIC so independently emitted objects link consistently across separate compilation.
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
            symbols: SymbolRegistry::default(),
            local_args: Vec::new(),
            parameter_slots: Vec::new(),
            frame_slot: None,
            entry_block: None,
            local_offsets: Vec::new(),
            arg_count: 0,
        };

        codegen.update_all(modules, extern_functions)?;

        // Verify the finished LLVM module; verifier failures indicate an internal compiler bug.
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
        self.parameter_slots.clear();
        self.arg_count = 0;
    }

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
        // Insert allocas before the entry block's first instruction, never at its current builder tail.
        match entry.get_first_instruction() {
            Some(first) => self.builder.position_before(&first),
            None => self.builder.position_at_end(entry),
        }
        let byte_array = self.context.i8_type().array_type(bytes.max(1));
        let slot = self
            .builder
            .build_alloca(byte_array, name)
            .expect("alloca always succeeds");
        if let Some(inst) = slot.as_instruction() {
            let _ = inst.set_alignment(align);
        }
        if let Some(block) = resume {
            self.builder.position_at_end(block);
        }
        slot
    }

    pub(super) fn ptr_type(&self) -> inkwell::types::PointerType<'ctx> {
        leaf::ptr_type(self.context)
    }

    pub(super) fn pointer_bytes(&self) -> u32 {
        self.target.pointer_bytes()
    }

    /// Object/assembly emission can fail on a validated target machine when
    /// user-authored inline assembly is rejected by the integrated
    /// assembler (bad instruction syntax, unknown register names, impossible
    /// constraints); that must surface as a normal compiler error, not a panic.
    fn finish(self) -> Result<EmitOutput, String> {
        match self.emit {
            EmitKind::Obj => {
                let buffer = self
                    .target_machine
                    .write_to_memory_buffer(&self.module, FileType::Object)
                    .map_err(|e| format!("failed to emit an object file: {e}"))?;
                Ok(EmitOutput::Object(buffer.as_slice().to_vec()))
            }
            EmitKind::Ir => Ok(EmitOutput::Text(self.module.print_to_string().to_string())),
            EmitKind::Asm => {
                let buffer = self
                    .target_machine
                    .write_to_memory_buffer(&self.module, FileType::Assembly)
                    .map_err(|e| format!("failed to emit assembly: {e}"))?;
                Ok(EmitOutput::Text(
                    String::from_utf8_lossy(buffer.as_slice()).into_owned(),
                ))
            }
        }
    }
}

pub(crate) fn generate(request: CodegenRequest) -> Result<EmitOutput, String> {
    let context = Context::create();
    Codegen::generate(&context, request).and_then(Codegen::finish)
}
