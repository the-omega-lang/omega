use crate::{EmitKind, OptLevel};
use omega_analyzer::Target;
use omega_analyzer::checked::ExternFunctionRef;
use omega_mir::MirModule;
use omega_parser::prelude::Ident;

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
    /// Retained for public API compatibility. MIR lowering consumes entry identity;
    /// native emission does not currently inspect this field.
    pub entry: Vec<Ident>,
    pub extern_functions: Vec<ExternFunctionRef>,
}
