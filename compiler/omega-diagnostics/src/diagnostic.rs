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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelStyle {
    Primary,
    Secondary,
}

#[derive(Debug, Clone)]
pub struct Label {
    pub style: LabelStyle,
    pub span: Span,
    pub message: String,
}

#[derive(Debug, Clone)]
pub enum Footer {
    Note(String),
    Help(String),
}

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    pub labels: Vec<Label>,
    pub footers: Vec<Footer>,
}

impl Diagnostic {
    pub fn new(severity: Severity, message: impl Into<String>) -> Self {
        Self {
            severity,
            message: message.into(),
            labels: Vec::new(),
            footers: Vec::new(),
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self::new(Severity::Error, message)
    }

    pub fn warning(message: impl Into<String>) -> Self {
        Self::new(Severity::Warning, message)
    }

    pub fn with_label(mut self, span: Span, message: impl Into<String>) -> Self {
        self.labels.push(Label {
            style: LabelStyle::Primary,
            span,
            message: message.into(),
        });
        self
    }

    pub fn with_secondary_label(mut self, span: Span, message: impl Into<String>) -> Self {
        self.labels.push(Label {
            style: LabelStyle::Secondary,
            span,
            message: message.into(),
        });
        self
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.footers.push(Footer::Note(note.into()));
        self
    }

    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.footers.push(Footer::Help(help.into()));
        self
    }

    pub fn primary_label(&self) -> Option<&Label> {
        self.labels
            .iter()
            .find(|l| l.style == LabelStyle::Primary)
            .or_else(|| self.labels.first())
    }
}
