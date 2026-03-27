use omega_parser::prelude::*;

fn main() {
    println!("[Omega Compiler]");

    let abc = StringExpr::parser().parse(
        r#####"
            """""this is my string"" it didnt end yet...
                """ also not yet... """" ok time to end it"""""
        "#####,
    );
    println!("abc: {:#?}", abc);

    // parse("extern puts : (fmt: *char) => i32;");
    // parse(r#""hello""#);
    // parse("{ a : i32; b : i32; c: u64; }");
    let ast = OmegaParser::parse_module(
        r###"

        extern puts : (fmt: *char) => i32;

        main(argc: i32, argv: **char) => void {
            a : i32;
            b : i32;

            puts("hello world!");
        }
        
    "###,
    )
    .expect("Failed to parse");

    println!("{:#?}", ast);
}
