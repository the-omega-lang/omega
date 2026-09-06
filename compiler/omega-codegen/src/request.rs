use crate::{EmitKind, OptLevel};
use omega_analyzer::Target;
use omega_analyzer::checked::ExternFunctionRef;
use omega_mir::EmissionUnit;
use omega_parser::prelude::Ident;
use std::path::PathBuf;

pub enum EmitOutput {
    Object(Vec<u8>),
    Text(String),
}

/// One emitted artifact, tagged with the physical source that owns it. The
/// source path is relative to the package root, so a caller reproduces the
/// on-disk tree by replacing its `.omg` extension with the emit kind's.
pub struct EmittedArtifact {
    pub source: PathBuf,
    pub output: EmitOutput,
}

pub struct CodegenRequest {
    pub target: Target,
    pub opt_level: OptLevel,
    pub emit: EmitKind,
    /// One unit per physical source file, in emission order.
    pub units: Vec<EmissionUnit>,
    /// Retained for public API compatibility. MIR lowering consumes entry identity;
    /// native emission does not currently inspect this field.
    pub entry: Vec<Ident>,
    pub extern_functions: Vec<ExternFunctionRef>,
}
