use omega_analyzer::analysis::{Analysis, Analyzer};
use omega_parser::{SourceModule, prelude::*};

fn main() {
    println!("[Omega Compiler]");

    let source = r###"
extern puts : (fmt: *char) => i32;

main(argc: i32, argv: **char) => void {
    a : i32;
    b : i32;

    puts("hello world!");
}

"###;
    println!("{}", source);
    let ast = SourceModule::parse(source).expect("Failed to parse");
    println!("{:#?}", ast);

    let analyzer = Analyzer::new();
    let object_module = analyzer.analyze(&ast).expect("Failed to analyze");
    println!("{:#?}", object_module);
}
