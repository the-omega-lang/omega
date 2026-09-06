use crate::cli::{self, Args, Command};
use omega_analyzer::Target;
use omega_codegen::{CodegenRequest, EmitKind, EmitOutput, EmittedArtifact};
use omega_diagnostics::{GREEN, Renderer, SourceRegistry, paint};
use omega_driver::{Driver, basename};
use omega_mir::EmissionSource;
use omega_parser::highlight::OmegaHighlighter;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::Instant;

pub(crate) enum AppError {
    Message(String),
    Reported,
}

impl From<String> for AppError {
    fn from(message: String) -> Self {
        Self::Message(message)
    }
}

pub(crate) fn run(raw_args: Vec<String>) -> Result<(), AppError> {
    match cli::parse(&raw_args)? {
        Command::Help => {
            cli::print_help();
            Ok(())
        }
        Command::Compile(args) => compile(args),
    }
}

fn compile(args: Args) -> Result<(), AppError> {
    let Args {
        entry_dir,
        output_dir,
        externs,
        name,
        opt_level,
        target,
        emit,
        verbose,
    } = args;
    let start = Instant::now();
    let colors = std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    let renderer = Renderer::new(colors).with_highlighter(Box::new(OmegaHighlighter));

    let entry_name = match name.clone() {
        Some(name) => name,
        None => {
            let physical_name = basename(&entry_dir).ok_or_else(|| {
                AppError::Message(format!(
                    "'{}' has no usable directory name -- a package root's own module file is \
                     named after its directory, so name the directory explicitly (a \
                     '<name>:<dir>' entry argument renames the module, it cannot supply a \
                     missing directory name)",
                    entry_dir.display()
                ))
            })?;
            cli::validate_module_name(
                physical_name.as_ref(),
                "inferred from the entry directory name; pass <name>:<dir> to override",
            )?
        }
    };

    if verbose {
        verbose_step(
            colors,
            "Compiling",
            &format!("{} ({target})", entry_dir.display()),
        );
    }

    let mut driver = Driver::new(entry_dir, name, externs, target).map_err(|errors| {
        render_driver_errors(&renderer, &errors);
        AppError::Reported
    })?;

    let program = driver
        .compile(&[entry_name.clone()], target)
        .map_err(|errors| {
            render_compile_errors(&renderer, &driver, &errors);
            AppError::Reported
        })?;

    for (module, warning) in &program.warnings {
        let source = driver.source_id(module);
        eprintln!(
            "{}\n",
            renderer.render(
                &warning.to_diagnostic().with_default_source(source),
                driver.sources()
            )
        );
    }

    if verbose {
        verbose_step(
            colors,
            "Compiled",
            &format!(
                "{} module(s), {} warning(s) in {:.2?}",
                program.modules.len(),
                program.warnings.len(),
                start.elapsed()
            ),
        );
        verbose_step(colors, "Lowering", "checked tree to MIR");
    }

    let mir_modules = omega_mir::lower_program(program.modules, &program.entry);
    let sources: Vec<EmissionSource> = program
        .sources
        .into_iter()
        .map(|source| EmissionSource {
            module: source.module,
            path: source.relative_path,
        })
        .collect();
    let units = omega_mir::plan_emission(mir_modules, &sources);

    if verbose {
        verbose_step(
            colors,
            "Generating",
            &format!("target {target}, LLVM, -O{opt_level}, emit {emit}"),
        );
    }

    let request = CodegenRequest {
        target,
        opt_level,
        emit,
        units,
        entry: program.entry.clone(),
        extern_functions: program.extern_functions,
    };
    let artifacts = omega_codegen::generate(request)?;

    if verbose {
        verbose_step(
            colors,
            "Emitting",
            &format!(
                "{} {} to {}/",
                artifacts.len(),
                if emit == EmitKind::Obj {
                    "object(s)"
                } else {
                    "text artifact(s)"
                },
                output_dir.display()
            ),
        );
    }

    write_artifacts(&output_dir, &artifacts, emit, target)?;

    if verbose {
        verbose_step(colors, "Finished", &format!("in {:.2?}", start.elapsed()));
    }
    println!(
        "Saved {} artifact(s) to: {}",
        artifacts.len(),
        output_dir.display()
    );
    Ok(())
}

/// Mirrors the package's source tree under `-o`, replacing each source's
/// `.omg` extension with the emit kind's. Existing artifacts are overwritten
/// in place: removing files the compiler does not own is a build system's
/// decision, not the compiler's.
fn write_artifacts(
    output_dir: &Path,
    artifacts: &[EmittedArtifact],
    emit: EmitKind,
    target: Target,
) -> Result<(), AppError> {
    if output_dir.exists() && !output_dir.is_dir() {
        return Err(AppError::Message(format!(
            "'{}' is not a directory -- '-o' names the output directory that receives one \
             artifact per source file",
            output_dir.display()
        )));
    }

    let extension = emit.extension(target);
    for artifact in artifacts {
        let path: PathBuf = output_dir.join(artifact.source.with_extension(extension));
        let write = match path.parent() {
            Some(parent) => std::fs::create_dir_all(parent),
            None => Ok(()),
        }
        .and_then(|()| match &artifact.output {
            EmitOutput::Object(bytes) => std::fs::write(&path, bytes),
            EmitOutput::Text(text) => std::fs::write(&path, text),
        });
        write.map_err(|error| {
            AppError::Message(format!("failed to write '{}': {error}", path.display()))
        })?;
    }
    Ok(())
}

fn render_driver_errors(renderer: &Renderer, errors: &[omega_driver::CompileError]) {
    let sources = SourceRegistry::default();
    for error in errors {
        for diagnostic in error.to_diagnostics() {
            eprintln!("{}\n", renderer.render(&diagnostic, &sources));
        }
    }
}

fn render_compile_errors(
    renderer: &Renderer,
    driver: &Driver,
    errors: &[omega_driver::CompileError],
) {
    let mut count = 0usize;
    for error in errors {
        let source = error.module().and_then(|module| driver.source_id(module));
        for diagnostic in error.to_diagnostics() {
            count += 1;
            eprintln!(
                "{}\n",
                renderer.render(&diagnostic.with_default_source(source), driver.sources())
            );
        }
    }

    let plural = if count == 1 { "error" } else { "errors" };
    let summary = omega_diagnostics::Diagnostic::error(format!(
        "could not compile the program due to {count} previous {plural}"
    ));
    eprintln!("{}", renderer.render(&summary, driver.sources()));
}

fn verbose_step(colors: bool, verb: &str, detail: &str) {
    eprintln!("{} {detail}", paint(colors, GREEN, &format!("{verb:>12}")));
}
