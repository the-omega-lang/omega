mod app;
mod cli;

use omega_diagnostics::{Renderer, SourceRegistry};
use std::io::IsTerminal;

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
        if let app::AppError::Diagnostics(diagnostics) = error {
            let renderer = Renderer::new(use_colors(std::io::stderr()));
            let sources = SourceRegistry::default();
            let rendered: Vec<String> = diagnostics
                .iter()
                .map(|diagnostic| renderer.render(diagnostic, &sources))
                .collect();
            eprintln!("{}", rendered.join("\n\n"));
        }
        std::process::exit(1);
    }
}

fn use_colors(stream: impl IsTerminal) -> bool {
    stream.is_terminal() && std::env::var_os("NO_COLOR").is_none()
}
