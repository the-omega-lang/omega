use std::io::{self, BufRead, Write};

fn write_demangled(line: &str, out: &mut impl Write) -> io::Result<()> {
    let symbol = line.trim();
    match omega_mangle::demangle(symbol) {
        Some(demangled) => writeln!(out, "{demangled}"),
        None => writeln!(out, "{symbol}"),
    }
}

fn main() -> io::Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let stdout = io::stdout();
    let mut out = stdout.lock();

    if args.is_empty() {
        for line in io::stdin().lock().lines() {
            write_demangled(&line?, &mut out)?;
        }
    } else {
        for arg in args {
            write_demangled(&arg, &mut out)?;
        }
    }

    Ok(())
}
