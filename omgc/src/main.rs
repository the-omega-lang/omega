use omega_parser::prelude::*;

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
    let ast = OmegaParser::parse_module(source).expect("Failed to parse");

    println!("{:#?}", ast);
}
