//! Turns a whole compiled program (one [`omega_mir::MirModule`] per source
//! module, already fully monomorphized -- see `omega_mir::lower_program`)
//! into final output, through whichever backend [`BackendKind`] selects.
//! Only one backend exists today (Cranelift, see the `cranelift` module),
//! gated behind this crate's own `cranelift` Cargo feature so a future
//! second backend can be added -- its own module, its own feature, one
//! more match arm in `generate` -- without the first one paying for it.
//!
//! Everything backend-agnostic lives at the crate root or in a shared
//! module: [`Target`] (a compilation target, in Omega's own vocabulary --
//! see `target`'s own doc comment), `mangle` (linker symbol names), and
//! `layout` (struct/enum/union byte layout -- the actual multi-backend
//! enabler; see its own doc comment for why this used to be welded to
//! Cranelift and no longer is).

#[cfg(feature = "cranelift")]
mod cranelift;
mod layout;
mod mangle;
mod target;

pub use target::{Arch, Os, Target, TargetParseError};

use omega_analyzer::checked::ExternFunctionRef;
use omega_mir::MirModule;
use omega_parser::prelude::Ident;
use std::fmt;

/// How aggressively a backend optimizes the generated code -- `-O<n>`.
/// Backend-agnostic by design (every native codegen library has *some*
/// notion of "how hard to try"); how a specific level maps onto a
/// specific backend's own settings is that backend's own business (see
/// `cranelift::cranelift_opt_setting`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OptLevel {
    #[default]
    O0,
    O1,
    O2,
    O3,
}

/// What [`generate`] should produce -- see [`EmitOutput`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EmitKind {
    #[default]
    Obj,
    /// The backend's own textual IR for every function -- backend-
    /// dependent by nature (Cranelift's own CLIF text today; a future
    /// non-Cranelift backend would have its own IR, or none at all).
    Ir,
    /// The backend's own per-target instruction listing for every
    /// function.
    Asm,
}

/// [`generate`]'s result -- an object file's bytes for [`EmitKind::Obj`],
/// or human-readable text (IR/assembly, one section per function) for
/// [`EmitKind::Ir`]/[`EmitKind::Asm`]. The caller (`omgc`) writes either
/// straight to the output path via `std::fs::write`, which accepts both.
pub enum EmitOutput {
    Object(Vec<u8>),
    Text(String),
}

/// Everything a backend needs to turn a whole compiled program into final
/// output -- the same shape every backend consumes, regardless of which
/// native codegen library drives it. Bundled into one named-field struct
/// (rather than passed as a long positional argument list) so a future
/// caller can't accidentally transpose two same-typed fields (`target`/
/// `entry` are both simple to mix up positionally, for instance).
pub struct CodegenRequest {
    pub module_name: String,
    pub target: Target,
    pub opt_level: OptLevel,
    pub emit: EmitKind,
    pub modules: Vec<(Vec<Ident>, MirModule)>,
    pub entry: Vec<Ident>,
    pub extern_functions: Vec<ExternFunctionRef>,
}

/// Which backend [`generate`] should drive -- one variant per Cargo
/// feature this crate enables (see `Cargo.toml`'s `[features]`), so a
/// backend nobody compiled in isn't even a choice the type system offers.
/// Only `Cranelift` exists today; adding a second backend is: its own
/// module + feature + one more arm each in `parse`/`Display`/`generate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    #[cfg(feature = "cranelift")]
    Cranelift,
}

impl BackendKind {
    /// Every backend name this build of the compiler actually supports,
    /// in the order `parse`/`--help` lists them -- kept as one array so
    /// the two never drift apart.
    pub const ALL: &'static [BackendKind] = &[
        #[cfg(feature = "cranelift")]
        BackendKind::Cranelift,
    ];

    pub fn parse(name: &str) -> Result<Self, String> {
        Self::ALL.iter().copied().find(|backend| backend.name() == name).ok_or_else(|| {
            let available: Vec<&str> = Self::ALL.iter().map(|b| b.name()).collect();
            format!("unknown backend '{name}' (available: {})", available.join(", "))
        })
    }

    fn name(self) -> &'static str {
        match self {
            #[cfg(feature = "cranelift")]
            BackendKind::Cranelift => "cranelift",
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

/// Turns `request` into final output through `backend`. The only fallible
/// step is target/ISA construction (a `--target` this build of the
/// compiler -- or the backend itself -- can't support comes back as a
/// plain `String`, matching `omgc`'s own CLI-error convention) or a
/// genuine within-program symbol collision (`@mangling(disabled)` used on
/// two functions with the same name); there is no other rejectable
/// *program* input left by the time this runs, since everything else was
/// already enforced while building the checked tree these `MirModule`s
/// were lowered from.
pub fn generate(backend: BackendKind, request: CodegenRequest) -> Result<EmitOutput, String> {
    match backend {
        #[cfg(feature = "cranelift")]
        BackendKind::Cranelift => cranelift::generate(request),
    }
}
