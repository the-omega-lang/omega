mod abi;
mod llvm;
mod options;
mod preflight;
mod request;
mod storage;
mod symbol;

pub use abi::{AbiReturn, AbiSignature, variadic_promotion};
pub use options::{EmitKind, OptLevel};
pub use request::{CodegenRequest, EmitOutput};

pub fn generate(request: CodegenRequest) -> Result<EmitOutput, String> {
    preflight::preflight(&request)?;
    if !llvm::supports(request.target) {
        return Err(format!(
            "target '{}' is not supported by this compiler's LLVM backend",
            request.target
        ));
    }

    llvm::generate(request)
}
