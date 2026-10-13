use crate::{EmitKind, OptLevel};
use omega_analyzer::Target;
use omega_analyzer::checked::{ExternFunctionRef, ExternGlobalRef};
use omega_analyzer::type_store::TypeStore;
use omega_mir::EmissionUnit;
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
    pub extern_functions: Vec<ExternFunctionRef>,
    pub extern_globals: Vec<ExternGlobalRef>,
    /// The frozen facts of every nominal type the units name.
    pub types: TypeStore,
}
