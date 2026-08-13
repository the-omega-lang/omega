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
    /// Two different package roots claim the same top-level module identity.
    /// Detected
    /// eagerly, before any module is parsed (see `ModuleRoots`), because the
    /// loser of such a collision would otherwise be silently unreachable,
    /// misrouting every reference to that name. Carries no module/span --
    /// this is about two *different* files at once, not one module's own
    /// source -- so it renders headline-only, like `MacroExpansion`.
    DuplicateModuleIdentity { name: Ident, first: PathBuf, second: PathBuf },
    /// Two `core` modules expose the same ambient macro name.
    AmbiguousPreludeMacro { name: Ident, first: ModulePath, second: ModulePath },
    /// A package root contains no modules at all. Reported here, before any
    /// analysis, rather than left to surface as an internal assertion later:
    /// `local_module_paths` returning nothing used to reach `compile`'s
    /// generic-instantiation merge and panic on its
    /// "always includes at least the entry module" expectation.
    ///
    /// The reachable cause is a root whose own module file is missing while a
    /// *directory* of the same name sits beside it -- the pre-migration
    /// `<root>/<name>/<name>.omg` layout, whose inner directory discovery
    /// deliberately skips (see `fs_resolve::discover_into`'s `skip`) so a
    /// directory-shaped module's own file is not double-counted as its own
    /// child. Carries no module or span: there is no module to anchor to,
    /// which is the whole problem, so it renders headline-only like
    /// `DuplicateModuleIdentity`.
    EmptyPackage { root: PathBuf, expected: PathBuf },
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
            Self::DuplicateModuleIdentity { .. }
            | Self::AmbiguousPreludeMacro { .. }
            | Self::EmptyPackage { .. } => None,
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
                "module identity '{}' is claimed by two different package roots: '{}' and '{}' -- \
                 give one an explicit name to disambiguate",
                name.as_ref(),
                first.display(),
                second.display(),
            ))],
            Self::EmptyPackage { root, expected } => vec![
                Diagnostic::error(format!(
                    "package root '{}' contains no modules -- expected its own module file at '{}'",
                    root.display(),
                    expected.display(),
                ))
                .with_help(
                    "a package root is its own root module, so its file is named after the \
                     directory and sits directly inside it; if this package still uses the older \
                     nested layout, move '<root>/<name>/*.omg' up into '<root>/'",
                ),
            ],
            Self::AmbiguousPreludeMacro { name, first, second } => vec![Diagnostic::error(format!(
                "exposed macro '{}' is provided by both core modules '{}' and '{}'",
                name.as_ref(),
                first.iter().map(Ident::as_ref).collect::<Vec<_>>().join("::"),
                second.iter().map(Ident::as_ref).collect::<Vec<_>>().join("::"),
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
