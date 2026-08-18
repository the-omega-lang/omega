
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
    isa: Arc<dyn isa::TargetIsa>,
    target: Target,
    module: ObjectModule,
    functions: HashMap<HirId, FuncId>,
    ctx: codegen::Context,
    emit: EmitKind,
    captured_text: String,

    bytes: HashMap<String, DataId>,
    const_blobs: HashMap<String, DataId>,
    vtables: HashMap<Vec<HirId>, DataId>,
    globals: HashMap<HirId, DataId>,

    local_args: Vec<Vec<Value>>,
    frame_slot: Option<StackSlot>,
    local_offsets: Vec<u32>,
    arg_count: usize,

    declared_symbols: HashMap<String, HirId>,
    symbol_error: Option<String>,
}

fn cranelift_opt_setting(level: OptLevel) -> &'static str {
    match level {
        OptLevel::O0 => "none",
        OptLevel::O1 | OptLevel::O2 => "speed",
        OptLevel::O3 => "speed_and_size",
    }
}

impl Codegen {
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

    pub(super) fn pointer_bytes(&self) -> u32 {
        self.pointer_type().bytes()
    }

    fn finish(self) -> EmitOutput {
        match self.emit {
            EmitKind::Obj => EmitOutput::Object(self.module.finish().emit().unwrap()),
            EmitKind::Ir | EmitKind::Asm => EmitOutput::Text(self.captured_text),
        }
    }
}

pub(crate) fn supports(target: Target) -> bool {
    matches!(target.arch, Arch::X86_64 | Arch::Aarch64)
}

pub(crate) fn generate(request: CodegenRequest) -> Result<EmitOutput, String> {
    Ok(Codegen::generate(request)?.finish())
}

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
