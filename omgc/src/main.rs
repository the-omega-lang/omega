use omega_codegen::Codegen;
use omega_driver::Driver;
use omega_parser::prelude::Ident;
use std::path::PathBuf;

fn main() {
    println!("[Omega Compiler]");

    // The entry file's directory is the (today: only) search root -- see
    // `Driver`'s doc comment on why this is already a `Vec` even though it
    // has exactly one entry. No CLI argument parsing yet, matching every
    // other hardcoded path in this driver.
    let entry_dir = PathBuf::from("examples/dev");
    let entry_module = vec![Ident("main".to_string())];

    let mut driver = Driver::new(vec![entry_dir]);
    let program = match driver.compile(&entry_module) {
        Ok(program) => program,
        Err(errors) => {
            for error in &errors {
                eprintln!("error: {error}");
            }
            std::process::exit(1);
        }
    };

    for warning in &program.warnings {
        println!("warning: {warning}");
    }

    let modname = "hello";
    let codegen = Codegen::generate(modname, "x86_64-unknown-linux", program.modules, &program.entry);
    let object = codegen.emit_object();

    let output_file = format!("target/{modname}.o");
    std::fs::write(&output_file, object).expect("Failed to write object");
    println!("Saved object to: {output_file}");
}
