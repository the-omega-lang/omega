use omega_codegen::Codegen;
use omega_diagnostics::Renderer;
use omega_driver::Driver;
use omega_parser::highlight::OmegaHighlighter;
use omega_parser::prelude::Ident;
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

fn run() {
    println!("[Omega Compiler]");

    // The entry file's directory is the (today: only) search root -- see
    // `Driver`'s doc comment on why this is already a `Vec` even though it
    // has exactly one entry. No CLI argument parsing yet, matching every
    // other hardcoded path in this driver.
    let entry_dir = PathBuf::from("examples/dev");
    let entry_module = vec![Ident("main".to_string())];

    // Diagnostics go to stderr, colored only when stderr really is a
    // terminal (and the user hasn't opted out via the conventional
    // `NO_COLOR`) -- piping/redirecting output gets clean plain text.
    let colors = std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    let renderer = Renderer::new(colors).with_highlighter(Box::new(OmegaHighlighter));

    let mut driver = Driver::new(vec![entry_dir]);
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

    let modname = "hello";
    let codegen = Codegen::generate(modname, "x86_64-unknown-linux", program.modules, &program.entry);
    let object = codegen.emit_object();

    let output_file = format!("target/{modname}.o");
    std::fs::write(&output_file, object).expect("Failed to write object");
    println!("Saved object to: {output_file}");
}
