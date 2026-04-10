use omega_analyzer::analysis::{Analysis, Analyzer};
use omega_codegen::Codegen;
use omega_parser::{SourceModule, prelude::*};

fn main() {
    println!("[Omega Compiler]");

    let source = r###"
    extern puts : (fmt: *char) => i32;

    main(argc: i32, argv: **char) => i32 {
        puts("hello world!");
        return 123;
    }

    "###;
    //     let source = r###"
    // extern puts : (fmt: *char) => i32;
    // "###;
    println!("{}", source);
    let ast = SourceModule::parse(source).expect("Failed to parse");
    println!("{:#?}", ast);

    let analyzer = Analyzer::new();
    let analysis = analyzer.analyze(&ast).expect("Failed to analyze");
    println!("{:#?}", analysis);

    let modname = "hello";
    let codegen = Codegen::generate(modname, "x86_64-unknown-linux", ast, analysis);
    let object = codegen.emit_object().expect("Failed to codegen");

    let output_file = format!("target/{modname}.o");
    std::fs::write(&output_file, object).expect("Failed to write object");
    println!("Saved object to: {output_file}");
}
