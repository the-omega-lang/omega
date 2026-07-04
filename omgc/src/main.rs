use omega_analyzer::analysis::Analyzer;
use omega_codegen::Codegen;
use omega_hir::ModuleId;
use omega_parser::SourceModule;

fn main() {
    println!("[Omega Compiler]");

    let source =
        std::fs::read_to_string("examples/dev/main.omg").expect("Failed to read source file");

    let ast = SourceModule::parse(&source).expect("Failed to parse");

    let hir = omega_hir::lower_module(ModuleId(0), &ast);

    let analyzer = Analyzer::new();
    let checked = analyzer.analyze(&hir).expect("Failed to analyze");

    let modname = "hello";
    let codegen = Codegen::generate(modname, "x86_64-unknown-linux", checked);
    let object = codegen.emit_object();

    let output_file = format!("target/{modname}.o");
    std::fs::write(&output_file, object).expect("Failed to write object");
    println!("Saved object to: {output_file}");
}
