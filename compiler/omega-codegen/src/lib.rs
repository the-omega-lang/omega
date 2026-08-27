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
    // Whether a target can be emitted is decided by LLVM target-machine
    // construction inside `llvm::generate`, not by a support list kept here.
    llvm::generate(request)
}
