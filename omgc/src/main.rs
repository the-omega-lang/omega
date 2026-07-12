use omega_codegen::Codegen;
use omega_diagnostics::Renderer;
use omega_driver::Driver;
use omega_parser::highlight::OmegaHighlighter;
use omega_parser::prelude::Ident;
use std::collections::HashMap;
use std::io::IsTerminal;
use std::path::PathBuf;

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

/// One `--extern=<name>:<path>` flag, plus the entry file itself -- the
/// whole command line, parsed by hand (no argument-parsing dependency,
/// matching this workspace's hand-rolled-everything style).
struct Args {
    entry_file: PathBuf,
    extern_roots: HashMap<Ident, PathBuf>,
}

/// `omgc <entry-file> [--extern=<name>:<path>]...` -- the entry file is the
/// only positional argument; every `--extern` registers one external
/// project's own root under the name `import extern::<name>;` selects it
/// with (see `omega_driver::Driver`'s `extern_roots` doc comment).
fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut entry_file = None;
    let mut extern_roots = HashMap::new();

    for arg in args {
        if let Some(rest) = arg.strip_prefix("--extern=") {
            let Some((name, path)) = rest.split_once(':') else {
                return Err(format!("invalid --extern flag '{arg}': expected --extern=<name>:<path>"));
            };
            if name.is_empty() {
                return Err(format!("invalid --extern flag '{arg}': the name before ':' cannot be empty"));
            }
            extern_roots.insert(Ident(name.to_string()), PathBuf::from(path));
        } else if arg.starts_with('-') {
            return Err(format!("unknown flag '{arg}'"));
        } else if entry_file.is_some() {
            return Err(format!("unexpected extra argument '{arg}' (the entry file was already given)"));
        } else {
            entry_file = Some(PathBuf::from(arg));
        }
    }

    let entry_file = entry_file.ok_or_else(|| {
        "usage: omgc <entry-file> [--extern=<name>:<path>]...".to_string()
    })?;
    Ok(Args { entry_file, extern_roots })
}

fn run() {
    println!("[Omega Compiler]");

    let args: Vec<String> = std::env::args().skip(1).collect();
    let Args { entry_file, extern_roots } = match parse_args(&args) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("error: {message}");
            std::process::exit(1);
        }
    };

    // The entry module's own name is its file's stem -- `json_parser.omg`
    // becomes module path `["json_parser"]`, exactly the convention every
    // other module already follows (`mymodule/mymodule.omg` ->
    // `["mymodule"]`); its parent directory is the local project's search
    // root, the same relationship every other module's own directory has to
    // its search root.
    let Some(entry_name) = entry_file.file_stem().and_then(|s| s.to_str()) else {
        eprintln!("error: '{}' has no usable file name", entry_file.display());
        std::process::exit(1);
    };
    let entry_module = vec![Ident(entry_name.to_string())];
    let entry_dir = entry_file.parent().map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));

    // Diagnostics go to stderr, colored only when stderr really is a
    // terminal (and the user hasn't opted out via the conventional
    // `NO_COLOR`) -- piping/redirecting output gets clean plain text.
    let colors = std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    let renderer = Renderer::new(colors).with_highlighter(Box::new(OmegaHighlighter));

    let mut driver = Driver::new(vec![entry_dir], extern_roots);
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
            let summary =
                omega_diagnostics::Diagnostic::error(format!("could not compile the program due to {count} previous {plural}"));
            eprintln!("{}", renderer.render(&summary, None));
            std::process::exit(1);
        }
    };

    for (module, warning) in &program.warnings {
        let file = driver.source_file(module);
        eprintln!("{}\n", renderer.render(&warning.to_diagnostic(), file.as_deref()));
    }

    let modname = entry_name;
    let codegen =
        Codegen::generate(modname, "x86_64-unknown-linux", program.modules, &program.entry, program.extern_functions);
    let object = codegen.emit_object();

    let output_file = format!("target/{modname}.o");
    std::fs::write(&output_file, object).expect("Failed to write object");
    println!("Saved object to: {output_file}");
}
