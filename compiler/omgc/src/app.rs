use crate::cli::{self, Args, Command};
use omega_codegen::{CodegenRequest, EmitKind, EmitOutput};
use omega_diagnostics::{GREEN, Renderer, paint};
use omega_driver::{Driver, basename};
use omega_parser::highlight::OmegaHighlighter;
use std::io::IsTerminal;
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
        output_file,
        externs,
        name,
        opt_level,
        target,
        emit,
        backend,
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
                     named after its directory, so name the directory explicitly ('--name=' \
                     renames the module, it cannot supply a missing directory name)",
                    entry_dir.display()
                ))
            })?;
            cli::validate_module_name(
                physical_name.as_ref(),
                "inferred from the entry directory name; pass --name=<name> to override",
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
        let file = driver.source_file(module);
        eprintln!(
            "{}\n",
            renderer.render(&warning.to_diagnostic(), file.as_deref())
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

    if verbose {
        verbose_step(
            colors,
            "Generating",
            &format!("target {target}, backend {backend}, -O{opt_level}, emit {emit}"),
        );
    }

    let request = CodegenRequest {
        module_name: entry_name.to_string(),
        target,
        opt_level,
        emit,
        modules: mir_modules,
        entry: program.entry.clone(),
        extern_functions: program.extern_functions,
    };
    let output = omega_codegen::generate(backend, request)?;

    if verbose {
        verbose_step(
            colors,
            "Emitting",
            &format!(
                "{} to {}",
                if emit == EmitKind::Obj {
                    "object"
                } else {
                    "text"
                },
                output_file.display()
            ),
        );
    }

    match output {
        EmitOutput::Object(bytes) => std::fs::write(&output_file, bytes),
        EmitOutput::Text(text) => std::fs::write(&output_file, text),
    }
    .map_err(|error| {
        AppError::Message(format!(
            "failed to write '{}': {error}",
            output_file.display()
        ))
    })?;

    if verbose {
        verbose_step(colors, "Finished", &format!("in {:.2?}", start.elapsed()));
    }
    println!("Saved output to: {}", output_file.display());
    Ok(())
}

fn render_driver_errors(renderer: &Renderer, errors: &[omega_driver::CompileError]) {
    for error in errors {
        for diagnostic in error.to_diagnostics() {
            eprintln!("{}\n", renderer.render(&diagnostic, None));
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
        let file = error.module().and_then(|module| driver.source_file(module));
        for diagnostic in error.to_diagnostics() {
            count += 1;
            eprintln!("{}\n", renderer.render(&diagnostic, file.as_deref()));
        }
    }

    let plural = if count == 1 { "error" } else { "errors" };
    let summary = omega_diagnostics::Diagnostic::error(format!(
        "could not compile the program due to {count} previous {plural}"
    ));
    eprintln!("{}", renderer.render(&summary, None));
}

fn verbose_step(colors: bool, verb: &str, detail: &str) {
    eprintln!("{} {detail}", paint(colors, GREEN, &format!("{verb:>12}")));
}
