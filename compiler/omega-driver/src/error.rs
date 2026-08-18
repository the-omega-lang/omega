use crate::ModulePath;
use omega_analyzer::checked::{CheckedModule, ExternFunctionRef};
use omega_analyzer::error::{AnalysisError, AnalysisWarning};
use omega_analyzer::resolver::ResolveError;
use omega_diagnostics::{Diagnostic, Span};
use omega_parser::macros::MacroError;
use omega_parser::prelude::{Ident, ParseError};
use std::path::PathBuf;

pub type ImportSite = (ModulePath, Span);

#[derive(Debug)]
pub enum CompileError {
    Resolve {
        error: ResolveError,
        importer: Option<ImportSite>,
    },
    Parse {
        module: ModulePath,
        errors: Vec<ParseError>,
    },
    MacroExpansion {
        module: ModulePath,
        error: MacroError,
    },
    Analysis {
        module: ModulePath,
        errors: Vec<AnalysisError>,
    },
    DuplicateModuleIdentity {
        name: Ident,
        first: PathBuf,
        second: PathBuf,
    },
    AmbiguousPreludeMacro {
        name: Ident,
        first: ModulePath,
        second: ModulePath,
    },
    EmptyPackage {
        root: PathBuf,
        expected: PathBuf,
    },
}

impl CompileError {
    pub fn module(&self) -> Option<&[Ident]> {
        match self {
            Self::Resolve { importer, .. } => {
                importer.as_ref().map(|(module, _)| module.as_slice())
            }
            Self::Parse { module, .. }
            | Self::MacroExpansion { module, .. }
            | Self::Analysis { module, .. } => Some(module),
            Self::DuplicateModuleIdentity { .. }
            | Self::AmbiguousPreludeMacro { .. }
            | Self::EmptyPackage { .. } => None,
        }
    }

    pub fn to_diagnostics(&self) -> Vec<Diagnostic> {
        match self {
            Self::Resolve { error, importer } => {
                vec![omega_analyzer::error::resolve_error_diagnostic(
                    error,
                    importer.as_ref().map(|&(_, span)| span),
                )]
            }
            Self::Parse { errors, .. } => errors.iter().map(ParseError::to_diagnostic).collect(),
            Self::MacroExpansion { error, .. } => vec![Diagnostic::error(error.to_string())],
            Self::Analysis { errors, .. } => {
                errors.iter().map(AnalysisError::to_diagnostic).collect()
            }
            Self::DuplicateModuleIdentity {
                name,
                first,
                second,
            } => vec![Diagnostic::error(format!(
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
            Self::AmbiguousPreludeMacro {
                name,
                first,
                second,
            } => vec![Diagnostic::error(format!(
                "exposed macro '{}' is provided by both core modules '{}' and '{}'",
                name.as_ref(),
                first
                    .iter()
                    .map(Ident::as_ref)
                    .collect::<Vec<_>>()
                    .join("::"),
                second
                    .iter()
                    .map(Ident::as_ref)
                    .collect::<Vec<_>>()
                    .join("::"),
            ))],
        }
    }
}

pub struct CompiledProgram {
    pub modules: Vec<(ModulePath, CheckedModule)>,
    pub entry: ModulePath,
    pub warnings: Vec<(ModulePath, AnalysisWarning)>,
    pub extern_functions: Vec<ExternFunctionRef>,
}
