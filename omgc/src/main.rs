use omega_parser::parse;

fn main() {
    println!("[Omega Compiler]");
    parse("(a: i32, b: (c:i32) => void) => *i32");
}
