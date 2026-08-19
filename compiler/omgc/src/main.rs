mod app;
mod cli;

/// The whole pipeline recurses over the AST (parser, HIR lowering, analysis,
/// MIR), so grammar nesting depth costs native stack. The parser bounds that
/// depth, but later passes spend more stack per AST level, so the compiler
/// still runs on a deliberately large, lazily committed worker stack.
fn main() {
    let result = std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(|| app::run(std::env::args().skip(1).collect()))
        .expect("failed to spawn compiler thread")
        .join()
        .expect("compiler thread panicked");

    if let Err(error) = result {
        if let app::AppError::Message(message) = error {
            eprintln!("error: {message}");
        }
        std::process::exit(1);
    }
}
