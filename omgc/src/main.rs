use omega_parser::Omega;

fn main() {
    println!("[Omega Compiler]");
    // parse("extern puts : (fmt: *char) => i32;");
    // parse(r#""hello""#);
    // parse("{ a : i32; b : i32; c: u64; }");
    let ast = Omega::parse_module(
        r###"

        extern puts : (fmt: *char) => i32;

        main(argc: i32, argv: **char) => void {
            a : i32;
            b : i32;

            puts("hello world!");
        }
        
    "###,
    );

    println!("{:#?}", ast);
}
