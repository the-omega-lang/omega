use crate::span::Span;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

impl Severity {
    pub fn name(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
        }
    }
}

/// How a labeled span is drawn: the `Primary` label is *the* location of
/// the problem (rendered with `^^^` in the severity's color, and the one
/// the `--> file:line:col` header points at); `Secondary` labels are
/// supporting context ("first declared here", "expected because of this"),
/// rendered with `---` in a distinct color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelStyle {
    Primary,
    Secondary,
}

/// One annotated source region within a diagnostic. `message` may be empty
/// -- the underline is still drawn, just with nothing after it.
#[derive(Debug, Clone)]
pub struct Label {
    pub style: LabelStyle,
    pub span: Span,
    pub message: String,
}

/// One complete, self-contained finding, structured so a renderer (not the
/// error site) decides presentation: a headline `message`, any number of
/// labeled source spans, and optional `note:`/`help:` footers. Built with
/// the chainable constructors below, e.g.:
///
/// ```
/// # use omega_diagnostics::{Diagnostic, Span};
/// Diagnostic::error("mismatched types")
///     .with_label(Span::new(10, 15), "expected `i32`, found `*u8`")
///     .with_help("Omega has no implicit conversions; the operand types must match exactly");
/// ```
///
/// Every span in `labels` indexes into a single source file -- the one the
/// diagnostic is rendered against (see `Renderer::render`); diagnostics
/// never span multiple files (each compiler stage reports against the
/// module it's analyzing).
#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    pub labels: Vec<Label>,
    pub notes: Vec<String>,
    pub helps: Vec<String>,
}

impl Diagnostic {
    pub fn new(severity: Severity, message: impl Into<String>) -> Self {
        Self { severity, message: message.into(), labels: Vec::new(), notes: Vec::new(), helps: Vec::new() }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self::new(Severity::Error, message)
    }

    pub fn warning(message: impl Into<String>) -> Self {
        Self::new(Severity::Warning, message)
    }

    /// Adds the primary label -- where the `-->` header points. A diagnostic
    /// should have exactly one; with several, the first wins the header.
    pub fn with_label(mut self, span: Span, message: impl Into<String>) -> Self {
        self.labels.push(Label { style: LabelStyle::Primary, span, message: message.into() });
        self
    }

    pub fn with_secondary_label(mut self, span: Span, message: impl Into<String>) -> Self {
        self.labels.push(Label { style: LabelStyle::Secondary, span, message: message.into() });
        self
    }

    /// A `= note:` footer -- factual context ("this language has no implicit
    /// conversions"), as opposed to `help`'s actionable advice.
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    /// A `= help:` footer -- what the user can *do* about the problem.
    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.helps.push(help.into());
        self
    }

    /// The label the `--> file:line:col` header should point at: the first
    /// primary label, or failing that the first label of any style.
    pub fn primary_label(&self) -> Option<&Label> {
        self.labels
            .iter()
            .find(|l| l.style == LabelStyle::Primary)
            .or_else(|| self.labels.first())
    }
}
