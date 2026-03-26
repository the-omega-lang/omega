use omega_parser::parse;

fn main() {
    println!("[Omega Compiler]");
    // parse("extern puts : (fmt: *char) => i32;");
    // parse(r#""hello""#);
    // parse("{ a : i32; b : i32; c: u64; }");
    parse(
        r###"

        main(argc: i32, argv: **char) => void {
            a : i32;
            b : i32;

            puts("hello world!");
        }
        
    "###,
    )
}
