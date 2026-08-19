mod abi;
mod backend;
#[cfg(feature = "cranelift")]
mod cranelift;
#[cfg(feature = "llvm")]
mod llvm;
mod options;
mod preflight;
mod request;
mod storage;
mod symbol;

pub use abi::{AbiReturn, AbiSignature, variadic_promotion};
pub use backend::BackendKind;
pub use options::{EmitKind, OptLevel};
pub use request::{CodegenRequest, EmitOutput};

pub fn generate(backend: BackendKind, request: CodegenRequest) -> Result<EmitOutput, String> {
    preflight::preflight(&request)?;
    if !backend.supports(request.target) {
        return Err(format!(
            "target '{}' is not supported by the '{}' backend (supported architectures: {})",
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
