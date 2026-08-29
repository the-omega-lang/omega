mod diagnostic;
mod highlight;
mod render;
mod source;
mod span;

pub use diagnostic::{Diagnostic, Footer, Label, LabelStyle, Severity, SourceSpan};
pub use highlight::{Highlighter, TokenClass};
pub use render::{BLUE, BOLD, CYAN, GREEN, RED, RESET, Renderer, YELLOW, paint};
pub use source::{SourceFile, SourceId, SourceRegistry};
pub use span::Span;
