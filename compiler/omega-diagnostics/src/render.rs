use crate::diagnostic::{Diagnostic, Footer, LabelStyle, Severity};
use crate::highlight::Highlighter;
use crate::source::SourceFile;

mod snippet;
#[cfg(test)]
mod tests;

pub const RESET: &str = "\x1b[0m";
pub const BOLD: &str = "\x1b[1m";
pub const RED: &str = "\x1b[1;31m";
pub const YELLOW: &str = "\x1b[1;33m";
pub const GREEN: &str = "\x1b[1;32m";
pub const BLUE: &str = "\x1b[1;34m";
pub const CYAN: &str = "\x1b[1;36m";

pub fn paint(colors: bool, code: &str, text: &str) -> String {
    if colors && !text.is_empty() {
        format!("{code}{text}{RESET}")
    } else {
        text.to_string()
    }
}

pub struct Renderer {
    colors: bool,
    highlighter: Option<Box<dyn Highlighter>>,
}

impl Renderer {
    pub fn new(colors: bool) -> Self {
        Self {
            colors,
            highlighter: None,
        }
    }

    pub fn with_highlighter(mut self, highlighter: Box<dyn Highlighter>) -> Self {
        self.highlighter = Some(highlighter);
        self
    }

    pub fn render(&self, diagnostic: &Diagnostic, file: Option<&SourceFile>) -> String {
        let mut out = String::new();
        self.render_header(&mut out, diagnostic);

        let mut width = 0;
        if let Some(file) = file
            && !diagnostic.labels.is_empty()
        {
            width = self.render_snippet(&mut out, diagnostic, file);
            if !diagnostic.footers.is_empty() {
                out.push('\n');
                self.push_empty_gutter(&mut out, width);
            }
        }

        for footer in &diagnostic.footers {
            out.push('\n');
            match footer {
                Footer::Note(text) => self.render_footer(&mut out, width, "note", text),
                Footer::Help(text) => self.render_footer(&mut out, width, "help", text),
            }
        }
        out
    }

    fn paint(&self, code: &str, text: &str) -> String {
        paint(self.colors, code, text)
    }

    fn severity_color(&self, severity: Severity) -> &'static str {
        match severity {
            Severity::Error => RED,
            Severity::Warning => YELLOW,
        }
    }

    fn label_color(&self, severity: Severity, style: LabelStyle) -> &'static str {
        match style {
            LabelStyle::Primary => self.severity_color(severity),
            LabelStyle::Secondary => CYAN,
        }
    }

    fn render_header(&self, out: &mut String, diagnostic: &Diagnostic) {
        out.push_str(&self.paint(
            self.severity_color(diagnostic.severity),
            diagnostic.severity.name(),
        ));
        out.push_str(&self.paint(BOLD, &format!(": {}", diagnostic.message)));
    }

    fn push_empty_gutter(&self, out: &mut String, width: usize) {
        out.push_str(&self.paint(BLUE, &format!("{:>width$} |", "", width = width)));
    }

    fn render_footer(&self, out: &mut String, width: usize, kind: &str, text: &str) {
        if width > 0 {
            out.push_str(&" ".repeat(width + 1));
        }
        out.push_str(&self.paint(BLUE, "= "));
        out.push_str(&self.paint(BOLD, &format!("{kind}:")));
        out.push(' ');
        let indent = " ".repeat(width + 3 + kind.len() + 2);
        out.push_str(&text.replace('\n', &format!("\n{indent}")));
    }
}
