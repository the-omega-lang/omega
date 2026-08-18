
use std::io::{self, BufRead, Write};

fn print_demangled(line: &str, out: &mut impl Write) {
    let trimmed = line.trim();
    match omega_mangle::demangle(trimmed) {
        Some(demangled) => writeln!(out, "{demangled}").ok(),
        None => writeln!(out, "{trimmed}").ok(),
    };
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let stdout = io::stdout();
    let mut out = stdout.lock();

    if args.is_empty() {
        let stdin = io::stdin();
        for line in stdin.lock().lines() {
            let Ok(line) = line else { break };
            print_demangled(&line, &mut out);
        }
    } else {
        for arg in &args {
            print_demangled(arg, &mut out);
        }
    }
}
