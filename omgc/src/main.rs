use omega_codegen::Codegen;
use omega_diagnostics::Renderer;
use omega_driver::{Driver, ExternRoot};
use omega_parser::highlight::OmegaHighlighter;
use omega_parser::prelude::Ident;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

/// `omega-parser`'s grammar is a hand-written recursive-descent parser,
/// including a few genuinely stack-recursive shapes (e.g. `CodeblockExpr`'s
/// body parser recurses one native stack frame per statement in a block --
/// see its doc comment). A single large `main()` like
/// `examples/dev/main.omg`'s can get deep enough to exceed the platform's
/// default thread stack (commonly 8MiB), so the real work runs on a
/// dedicated thread with a much larger stack instead of the process's main
/// thread -- the same mitigation real-world recursive-descent compilers
/// commonly use, rather than a change to the grammar itself.
fn main() {
    std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(run)
        .expect("failed to spawn compiler thread")
        .join()
        .expect("compiler thread panicked");
}

/// One `--extern=<alias>:<file>` flag, plus the entry file itself -- the
/// whole command line, parsed by hand (no argument-parsing dependency,
/// matching this workspace's hand-rolled-everything style).
struct Args {
    entry_file: PathBuf,
    externs: Vec<ExternRoot>,
}

/// A module's own name (its file's stem) and search-root directory (its
/// parent) -- the convention every module already follows
/// (`mymodule/mymodule.omg` -> `["mymodule"]`), applied here to both the
/// entry file and every `--extern` target: an extern file is just an entry
/// file for someone else's project.
fn module_from_file(file: &Path) -> Option<(Ident, PathBuf)> {
    let name = file.file_stem()?.to_str()?.to_string();
    let dir = file
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    Some((Ident(name), dir))
}

/// `omgc <entry-file> [--extern=<alias>:<file>]...` -- the entry file is
/// the only positional argument; every `--extern` points *directly* at the
/// external project's own entry file (not a directory), so `<alias>` can
/// freely differ from that file's own name -- `import extern::<alias>;`
/// selects it, but the module path used internally (and for cross-process
/// symbol mangling) is always that file's own stem, exactly like the local
/// entry file's own name is (see `omega_driver::Driver`'s `extern_aliases`
/// doc comment).
fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut entry_file = None;
    let mut externs = Vec::new();

    for arg in args {
        if let Some(rest) = arg.strip_prefix("--extern=") {
            let Some((alias, file)) = rest.split_once(':') else {
                return Err(format!(
                    "invalid --extern flag '{arg}': expected --extern=<alias>:<file>"
                ));
            };
            if alias.is_empty() {
                return Err(format!(
                    "invalid --extern flag '{arg}': the alias before ':' cannot be empty"
                ));
            }
            let file = PathBuf::from(file);
            let Some((module, dir)) = module_from_file(&file) else {
                return Err(format!(
                    "invalid --extern flag '{arg}': '{}' has no usable file name",
                    file.display()
                ));
            };
            externs.push(ExternRoot {
                alias: Ident(alias.to_string()),
                dir,
                module,
            });
        } else if arg.starts_with('-') {
            return Err(format!("unknown flag '{arg}'"));
        } else if entry_file.is_some() {
            return Err(format!(
                "unexpected extra argument '{arg}' (the entry file was already given)"
            ));
        } else {
            entry_file = Some(PathBuf::from(arg));
        }
    }

    let entry_file = entry_file
        .ok_or_else(|| "usage: omgc <entry-file> [--extern=<alias>:<file>]...".to_string())?;
    Ok(Args {
        entry_file,
        externs,
    })
}

fn run() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Args {
        entry_file,
        externs,
    } = match parse_args(&args) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("error: {message}");
            std::process::exit(1);
        }
    };

    let Some((entry_name, entry_dir)) = module_from_file(&entry_file) else {
        eprintln!("error: '{}' has no usable file name", entry_file.display());
        std::process::exit(1);
    };
    let entry_module = vec![entry_name.clone()];

    // Diagnostics go to stderr, colored only when stderr really is a
    // terminal (and the user hasn't opted out via the conventional
    // `NO_COLOR`) -- piping/redirecting output gets clean plain text.
    let colors = std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    let renderer = Renderer::new(colors).with_highlighter(Box::new(OmegaHighlighter));

    let mut driver = Driver::new(vec![entry_dir], externs);
    let program = match driver.compile(&entry_module) {
        Ok(program) => program,
        Err(errors) => {
            let mut count = 0usize;
            for error in &errors {
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
            std::process::exit(1);
        }
    };

    for (module, warning) in &program.warnings {
        let file = driver.source_file(module);
        eprintln!(
            "{}\n",
            renderer.render(&warning.to_diagnostic(), file.as_deref())
        );
    }

    let modname = entry_name.as_ref();
    let codegen = Codegen::generate(
        modname,
        "x86_64-unknown-linux",
        program.modules,
        &program.entry,
        program.extern_functions,
    );
    let object = codegen.emit_object();

    let output_file = format!("target/{modname}.o");
    std::fs::write(&output_file, object).expect("Failed to write object");
}
