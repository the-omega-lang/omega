mod abi;
#[cfg(feature = "cranelift")]
mod cranelift;
#[cfg(feature = "llvm")]
mod llvm;
mod preflight;

pub use abi::{AbiReturn, AbiSignature, variadic_promotion};

use omega_analyzer::Target;

use omega_analyzer::checked::ExternFunctionRef;
use omega_mir::MirModule;
use omega_parser::prelude::Ident;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OptLevel {
    #[default]
    O0,
    O1,
    O2,
    O3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EmitKind {
    #[default]
    Obj,
    Ir,
    Asm,
}

pub enum EmitOutput {
    Object(Vec<u8>),
    Text(String),
}

pub struct CodegenRequest {
    pub module_name: String,
    pub target: Target,
    pub opt_level: OptLevel,
    pub emit: EmitKind,
    pub modules: Vec<(Vec<Ident>, MirModule)>,
    pub entry: Vec<Ident>,
    pub extern_functions: Vec<ExternFunctionRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    #[cfg(feature = "cranelift")]
    Cranelift,
    #[cfg(feature = "llvm")]
    Llvm,
}

impl BackendKind {
    pub const ALL: &'static [BackendKind] = &[
        #[cfg(feature = "cranelift")]
        BackendKind::Cranelift,
        #[cfg(feature = "llvm")]
        BackendKind::Llvm,
    ];

    pub fn parse(name: &str) -> Result<Self, String> {
        Self::ALL
            .iter()
            .copied()
            .find(|backend| backend.name() == name)
            .ok_or_else(|| {
                let available: Vec<&str> = Self::ALL.iter().map(|b| b.name()).collect();
                format!(
                    "unknown backend '{name}' (available: {})",
                    available.join(", ")
                )
            })
    }

    fn name(self) -> &'static str {
        match self {
            #[cfg(feature = "cranelift")]
            BackendKind::Cranelift => "cranelift",
            #[cfg(feature = "llvm")]
            BackendKind::Llvm => "llvm",
        }
    }

    pub fn supports(self, target: Target) -> bool {
        match self {
            #[cfg(feature = "cranelift")]
            BackendKind::Cranelift => cranelift::supports(target),
            #[cfg(feature = "llvm")]
            BackendKind::Llvm => llvm::supports(target),
        }
    }

    pub fn supported_targets(self) -> &'static str {
        match self {
            #[cfg(feature = "cranelift")]
            BackendKind::Cranelift => "x86_64, aarch64",
            #[cfg(feature = "llvm")]
            BackendKind::Llvm => "x86_64, x86, armv7, thumbv7em, aarch64, riscv32, riscv64",
        }
    }
}

impl fmt::Display for BackendKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(feature = "cranelift")]
impl Default for BackendKind {
    fn default() -> Self {
        BackendKind::Cranelift
    }
}

pub fn generate(backend: BackendKind, request: CodegenRequest) -> Result<EmitOutput, String> {
    preflight::preflight(&request)?;
    if !backend.supports(request.target) {
        return Err(format!(
            "target '{}' is not supported by the '{}' backend (supported: {})",
            request.target,
            backend,
            backend.supported_targets()
        ));
    }
    match backend {
        #[cfg(feature = "cranelift")]
        BackendKind::Cranelift => cranelift::generate(request),
        #[cfg(feature = "llvm")]
        BackendKind::Llvm => llvm::generate(request),
    }
}
