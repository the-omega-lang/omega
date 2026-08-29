use crate::source::SourceId;
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

/// A byte range together with the file it indexes. Any location that may
/// outlive the module currently being analyzed must be one of these, so a
/// later renderer cannot read it against the wrong source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SourceSpan {
    pub source: SourceId,
    pub span: Span,
}

impl SourceSpan {
    pub const fn new(source: SourceId, span: Span) -> Self {
        Self { source, span }
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
    /// `None` defers to the diagnostic's own source, which is how a finding
    /// produced inside one module stays source-agnostic until the driver
    /// stamps the module it came from.
    pub source: Option<SourceId>,
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
    pub source: Option<SourceId>,
    pub labels: Vec<Label>,
    pub footers: Vec<Footer>,
}

impl Diagnostic {
    pub fn new(severity: Severity, message: impl Into<String>) -> Self {
        Self {
            severity,
            message: message.into(),
            source: None,
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

    /// Names the file that unqualified labels index. Labels that already name
    /// their own source keep it.
    pub fn in_source(mut self, source: impl Into<Option<SourceId>>) -> Self {
        self.source = source.into();
        self
    }

    /// Supplies the owning file at the rendering boundary without overriding a
    /// diagnostic that already knows which source it was built against.
    pub fn with_default_source(mut self, source: Option<SourceId>) -> Self {
        self.source = self.source.or(source);
        self
    }

    pub fn with_label(self, span: Span, message: impl Into<String>) -> Self {
        self.push_label(LabelStyle::Primary, None, span, message)
    }

    pub fn with_secondary_label(self, span: Span, message: impl Into<String>) -> Self {
        self.push_label(LabelStyle::Secondary, None, span, message)
    }

    pub fn with_label_in(self, at: SourceSpan, message: impl Into<String>) -> Self {
        self.push_label(LabelStyle::Primary, Some(at.source), at.span, message)
    }

    pub fn with_secondary_label_in(self, at: SourceSpan, message: impl Into<String>) -> Self {
        self.push_label(LabelStyle::Secondary, Some(at.source), at.span, message)
    }

    fn push_label(
        mut self,
        style: LabelStyle,
        source: Option<SourceId>,
        span: Span,
        message: impl Into<String>,
    ) -> Self {
        self.labels.push(Label {
            style,
            source,
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
