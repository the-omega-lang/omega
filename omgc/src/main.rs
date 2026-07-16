use omega_codegen::{Codegen, EmitKind, EmitOutput, OptLevel, Target};
use omega_diagnostics::{BOLD, CYAN, GREEN, Renderer, paint};
use omega_driver::{Driver, ExternRoot};
use omega_parser::highlight::OmegaHighlighter;
use omega_parser::prelude::Ident;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::Instant;

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

/// The whole command line, parsed by hand (no argument-parsing dependency,
/// matching this workspace's hand-rolled-everything style). `-h`/`--help`
/// is handled separately, before this ever runs (see `run`) -- everything
/// here assumes a real compile was actually requested.
struct Args {
    entry_file: PathBuf,
    /// `-o <file>` -- required, no default (unlike every flag below, which
    /// falls back to today's previously-hardcoded behavior when omitted).
    output_file: PathBuf,
    externs: Vec<ExternRoot>,
    /// `--name=<name>` -- overrides the entry module's own declared
    /// identity; `None` (the default) keeps `module_from_file`'s
    /// stem-derived name.
    name: Option<Ident>,
    opt_level: OptLevel,
    target: Target,
    emit: EmitKind,
    verbose: bool,
}

/// A module's own *default* identity (its file's stem) and search-root
/// directory (its parent) -- the convention every module follows unless
/// explicitly overridden, applied here to both the entry file and every
/// `--extern` target before any `--name=`/explicit `--extern=<name>:<file>`
/// name is applied on top (see `parse_args`): an extern file is just an
/// entry file for someone else's project.
fn module_from_file(file: &Path) -> Option<(Ident, PathBuf)> {
    let name = file.file_stem()?.to_str()?.to_string();
    let dir = file
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    Some((Ident(name), dir))
}

/// `omgc <entry-file> -o <output-file> [OPTIONS]` -- the entry file is the
/// only positional argument; `-o` is a separate next-token argument (unlike
/// every other flag here, which is `=`-attached or bare), so this walks
/// `args` with an explicit iterator rather than a plain `for` loop, to
/// consume the token following `-o` on demand.
fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut entry_file = None;
    let mut output_file = None;
    let mut externs = Vec::new();
    let mut name = None;
    let mut opt_level = OptLevel::default();
    let mut target = Target::DEFAULT;
    let mut emit = EmitKind::default();
    let mut verbose = false;

    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if let Some(rest) = arg.strip_prefix("--extern=") {
            // Two shapes: a bare `--extern=<file>` (the common case --
            // identity inferred from the file's own stem, exactly like the
            // entry file), or an explicit `--extern=<name>:<file>` (the
            // name is author-stated and used exactly as typed -- never
            // re-derived, never a separate translated alias). Distinguished
            // by whether `rest` contains a `:` at all; `split_once` takes
            // only the *first* one, so a file path containing later colons
            // still parses correctly in the explicit form, same as before.
            let (explicit_name, file) = match rest.split_once(':') {
                Some((name, file)) => {
                    if name.is_empty() {
                        return Err(format!(
                            "invalid --extern flag '{arg}': the name before ':' cannot be empty"
                        ));
                    }
                    (Some(Ident(name.to_string())), PathBuf::from(file))
                }
                None => (None, PathBuf::from(rest)),
            };
            let Some((inferred_name, dir)) = module_from_file(&file) else {
                return Err(format!(
                    "invalid --extern flag '{arg}': '{}' has no usable file name",
                    file.display()
                ));
            };
            externs.push(ExternRoot {
                name: explicit_name.unwrap_or(inferred_name),
                dir,
                file,
            });
        } else if let Some(rest) = arg.strip_prefix("--name=") {
            if rest.is_empty() {
                return Err(format!("invalid --name flag '{arg}': the name cannot be empty"));
            }
            name = Some(Ident(rest.to_string()));
        } else if arg == "-o" {
            let file = iter.next().ok_or_else(|| "expected a file path after '-o'".to_string())?;
            output_file = Some(PathBuf::from(file));
        } else if let Some(rest) = arg.strip_prefix("-O") {
            opt_level = match rest {
                "0" => OptLevel::O0,
                "1" => OptLevel::O1,
                "2" => OptLevel::O2,
                "3" => OptLevel::O3,
                other => {
                    return Err(format!(
                        "invalid optimization level '-O{other}': expected -O0, -O1, -O2, or -O3"
                    ));
                }
            };
        } else if let Some(rest) = arg.strip_prefix("--target=") {
            target = Target::parse(rest).map_err(|e| e.to_string())?;
        } else if let Some(rest) = arg.strip_prefix("--emit=") {
            emit = match rest {
                "obj" => EmitKind::Obj,
                "ir" => EmitKind::Ir,
                "asm" => EmitKind::Asm,
                other => return Err(format!("invalid --emit value '{other}': expected obj, ir, or asm")),
            };
        } else if arg == "-v" || arg == "--verbose" {
            verbose = true;
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
        .ok_or_else(|| "usage: omgc <entry-file> -o <output-file> [OPTIONS] (see --help)".to_string())?;
    let output_file = output_file.ok_or_else(|| "the -o <file> flag is required".to_string())?;
    Ok(Args { entry_file, output_file, externs, name, opt_level, target, emit, verbose })
}

/// One `-h`/`--help` line: `flag` padded to a fixed column *before* being
/// colored (padding an already-escape-coded string would count the
/// invisible ANSI bytes toward its width and misalign every row).
fn help_option(colors: bool, flag: &str, desc: &str) {
    let padded = format!("{flag:<26}");
    println!("    {} {desc}", paint(colors, CYAN, &padded));
}

/// Prints to stdout (colored based on *stdout's* own terminal-ness,
/// independent of the stderr-based `colors` diagnostics/verbose output
/// use) and exits -- checked before any other argument parsing, so
/// `omgc -h` alone works with no entry file or `-o`, standard CLI
/// convention.
fn print_help() {
    let colors = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    println!("{}", paint(colors, BOLD, "omgc"));
    println!("The Omega compiler\n");
    println!("{}", paint(colors, BOLD, "USAGE:"));
    println!("    omgc <entry-file> -o <output-file> [OPTIONS]\n");
    println!("{}", paint(colors, BOLD, "OPTIONS:"));
    help_option(colors, "-o <file>", "Output file path (required)");
    help_option(colors, "-O<0-3>", "Optimization level (default: 0)");
    help_option(
        colors,
        "--target=<triplet>",
        &format!("Target triplet, e.g. x86_64-unknown-linux (default: {})", Target::DEFAULT),
    );
    help_option(colors, "--emit=<obj|ir|asm>", "What to emit: object file (default), Cranelift IR, or assembly");
    help_option(
        colors,
        "--extern=[<name>:]<file>",
        "Register an external module (name inferred from the file by default, repeatable)",
    );
    help_option(colors, "--name=<name>", "Override the entry module's own identity (default: derived from its file name)");
    help_option(colors, "-v, --verbose", "Print progress information");
    help_option(colors, "-h, --help", "Print this help message");
}

/// One progress line, styled like Cargo's own `{bold green}{verb:>12}{reset}
/// {detail}` convention -- `-v`/`--verbose` only.
fn verbose_step(colors: bool, verb: &str, detail: &str) {
    eprintln!("{} {detail}", paint(colors, GREEN, &format!("{verb:>12}")));
}

fn run() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "-h" || a == "--help") {
        print_help();
        return;
    }

    let start = Instant::now();
    let Args { entry_file, output_file, externs, name, opt_level, target, emit, verbose } = match parse_args(&args) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("error: {message}");
            std::process::exit(1);
        }
    };

    let Some((stem_name, entry_dir)) = module_from_file(&entry_file) else {
        eprintln!("error: '{}' has no usable file name", entry_file.display());
        std::process::exit(1);
    };
    let entry_name = name.unwrap_or(stem_name);
    let entry_module = vec![entry_name.clone()];

    // Diagnostics (and verbose output, which shares the same stream) go to
    // stderr, colored only when stderr really is a terminal (and the user
    // hasn't opted out via the conventional `NO_COLOR`) -- piping/
    // redirecting output gets clean plain text.
    let colors = std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    let renderer = Renderer::new(colors).with_highlighter(Box::new(OmegaHighlighter));

    if verbose {
        verbose_step(colors, "Compiling", &format!("{} ({target})", entry_file.display()));
    }

    let mut driver = Driver::new(vec![entry_dir], externs);
    let program = match driver.compile(&entry_module, &entry_file) {
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

    if verbose {
        verbose_step(
            colors,
            "Compiled",
            &format!("{} module(s), {} warning(s) in {:.2?}", program.modules.len(), program.warnings.len(), start.elapsed()),
        );
        verbose_step(colors, "Generating", &format!("target {target}, opt level {opt_level:?}, emit {emit:?}"));
    }

    let modname = entry_name.as_ref();
    let codegen = match Codegen::generate(modname, target, opt_level, emit, program.modules, &program.entry, program.extern_functions) {
        Ok(codegen) => codegen,
        Err(message) => {
            eprintln!("error: {message}");
            std::process::exit(1);
        }
    };

    if verbose {
        verbose_step(colors, "Emitting", &format!("{} to {}", if emit == EmitKind::Obj { "object" } else { "text" }, output_file.display()));
    }

    let write_result = match codegen.finish() {
        EmitOutput::Object(bytes) => std::fs::write(&output_file, bytes),
        EmitOutput::Text(text) => std::fs::write(&output_file, text),
    };
    if let Err(err) = write_result {
        eprintln!("error: failed to write '{}': {err}", output_file.display());
        std::process::exit(1);
    }

    if verbose {
        verbose_step(colors, "Finished", &format!("in {:.2?}", start.elapsed()));
    }
    println!("Saved output to: {}", output_file.display());
}
