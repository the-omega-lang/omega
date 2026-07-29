//! What a compilation can fail with, and what it produces when it doesn't.

use crate::ModulePath;
use omega_analyzer::checked::{CheckedModule, ExternFunctionRef};
use omega_analyzer::error::{AnalysisError, AnalysisWarning};
use omega_analyzer::resolver::ResolveError;
use omega_diagnostics::{Diagnostic, Span};
use omega_parser::macros::MacroError;
use omega_parser::prelude::{Ident, ParseError};
use std::path::PathBuf;

/// The module and `import` statement that pulled a failing module in, when
/// one is known -- every resolution failure found during reachability
/// discovery has one; only a broken *entry* module doesn't.
pub type ImportSite = (ModulePath, Span);

/// Everything that can go wrong compiling a multi-module program, kept fully
/// structured (never pre-rendered strings) so the CLI can render each finding
/// as an annotated source snippet -- see [`CompileError::module`]/
/// [`CompileError::to_diagnostics`] and `Driver::source_file`.
#[derive(Debug)]
pub enum CompileError {
    /// A module-resolution failure, tagged with the referencing site.
    Resolve { error: ResolveError, importer: Option<ImportSite> },
    /// Syntax errors in one module's own source file.
    Parse { module: ModulePath, errors: Vec<ParseError> },
    /// The module parsed, but macro expansion (run right after parsing,
    /// before HIR lowering) failed.
    MacroExpansion { module: ModulePath, error: MacroError },
    /// Ordinary semantic errors from one module's own signature/body
    /// analysis.
    Analysis { module: ModulePath, errors: Vec<AnalysisError> },
    /// Two different files both claim the same top-level module identity --
    /// the entry's own real name, or a `--extern`'s, collide. Detected
    /// eagerly, before any module is parsed (see `ModuleRoots`), because the
    /// loser of such a collision would otherwise be silently unreachable,
    /// misrouting every reference to that name. Carries no module/span --
    /// this is about two *different* files at once, not one module's own
    /// source -- so it renders headline-only, like `MacroExpansion`.
    DuplicateModuleIdentity { name: Ident, first: PathBuf, second: PathBuf },
}

impl CompileError {
    /// The module whose source file this error's diagnostics render against
    /// -- `None` when there is no single such module (see the variants).
    pub fn module(&self) -> Option<&[Ident]> {
        match self {
            Self::Resolve { importer, .. } => importer.as_ref().map(|(module, _)| module.as_slice()),
            Self::Parse { module, .. } | Self::MacroExpansion { module, .. } | Self::Analysis { module, .. } => {
                Some(module)
            }
            Self::DuplicateModuleIdentity { .. } => None,
        }
    }

    pub fn to_diagnostics(&self) -> Vec<Diagnostic> {
        match self {
            Self::Resolve { error, importer } => {
                vec![omega_analyzer::error::resolve_error_diagnostic(error, importer.as_ref().map(|&(_, span)| span))]
            }
            Self::Parse { errors, .. } => errors.iter().map(ParseError::to_diagnostic).collect(),
            // A macro error carries no span today (macro expansion runs on
            // spliced token streams, where "one location" is genuinely
            // ambiguous -- definition site vs. invocation site); it renders
            // as a headline-only diagnostic.
            Self::MacroExpansion { error, .. } => vec![Diagnostic::error(error.to_string())],
            Self::Analysis { errors, .. } => errors.iter().map(AnalysisError::to_diagnostic).collect(),
            Self::DuplicateModuleIdentity { name, first, second } => vec![Diagnostic::error(format!(
                "module identity '{}' is claimed by two different --extern directories: '{}' and '{}' -- \
                 give one an explicit --extern=<name>:<dir> to disambiguate",
                name.as_ref(),
                first.display(),
                second.display(),
            ))],
        }
    }
}

/// The result of compiling every module reachable from `entry`: each one's
/// `CheckedModule`, tagged with its absolute module path (codegen needs both
/// for cross-module symbol mangling), plus every non-fatal finding across all
/// of them.
pub struct CompiledProgram {
    pub modules: Vec<(ModulePath, CheckedModule)>,
    pub entry: ModulePath,
    /// Each warning tagged with the module it was found in, so the CLI can
    /// render it against the right source file.
    pub warnings: Vec<(ModulePath, AnalysisWarning)>,
    /// Every extern-owned function/method this compilation actually
    /// referenced (see `Driver::collect_extern_functions`) -- `modules` never
    /// contains a body for any of these (an extern module's ordinary items
    /// are scanned, never compiled), so codegen must declare each one itself,
    /// `Linkage::Import`-only, trusting that the *other* `omgc` invocation
    /// compiling that module standalone produces the exact same mangled
    /// symbol (a deterministic function of module path + name + the item's
    /// own per-module `HirId.local`).
    ///
    /// That trust has one precondition: both invocations must agree on the
    /// module's declared identity (`--name=`/`--extern=<name>:<file>`) --
    /// automatic when neither side overrides it. If they disagree, the two
    /// mangled symbols diverge and the link step fails loudly (undefined
    /// symbol) rather than anything more dangerous.
    pub extern_functions: Vec<ExternFunctionRef>,
}
