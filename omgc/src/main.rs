use omega_analyzer::analysis::{Analysis, Analyzer};
use omega_codegen::Codegen;
use omega_parser::{SourceModule, prelude::*};

fn main() {
    println!("[Omega Compiler]");

    let source = r###"
    extern puts : (fmt: *char) => i32;

    print_message(msg: *char) => i32 {
        return puts(msg);
    }

    main(argc: i32, argv: **char) => i32 {
        a : i32;
        a = 69;
        msg : *char;
        msg = "hello worlderino";
        print_message(msg);
        return a;
    }

    "###;

    // TEST
    // let source = "hello()";
    // println!("{}", source);
    // let parser = FunctionCallExpr::parser(ExpressionNode::configured_parser());
    // println!("Parsed: {:?}", parser.parse(source).unwrap());
    // return;
    // ENDTEST

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
