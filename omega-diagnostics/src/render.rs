//! The terminal renderer: turns one [`Diagnostic`] plus its [`SourceFile`]
//! into a Rust-style annotated snippet --
//!
//! ```text
//! error: mismatched types
//!   --> examples/dev/main.omg:88:13
//!    |
//! 88 |     total = total + "hi";
//!    |             ^^^^^^^^^^^^ expected `i32`, found `*u8`
//!    |
//!    = help: Omega has no implicit conversions; the operand types must match
//! ```
//!
//! Layout rules follow rustc's renderer closely (it's the convention users
//! already know how to read): a severity-colored headline, a `-->` location
//! header pointing at the primary label, a `|`-guttered snippet with one
//! underline row per label (`^^^` primary, `---` secondary), and
//! `= note:`/`= help:` footers. Multi-line spans get the bracket form:
//!
//! ```text
//! 12 |   testvar := if x {
//!    |  ____________^
//! 13 | |     "yes"
//! 14 | | };
//!    | |_^ the branches disagree
//! ```
//!
//! Labels render as independent, sequentially-emitted blocks (sorted by
//! start offset) rather than being merged into one shared multi-column
//! layout -- overlapping multi-line labels therefore repeat their shared
//! lines instead of stacking extra bar columns, trading rustc's last few
//! percent of polish for a renderer that stays simple and can never
//! produce a misaligned layout on pathological input.

use crate::diagnostic::{Diagnostic, Label, LabelStyle, Severity};
use crate::highlight::{Highlighter, TokenClass};
use crate::source::SourceFile;
use crate::span::Span;

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const RED: &str = "\x1b[1;31m";
const YELLOW: &str = "\x1b[1;33m";
const BLUE: &str = "\x1b[1;34m";
const CYAN: &str = "\x1b[1;36m";

const SYNTAX_KEYWORD: &str = "\x1b[35m";
const SYNTAX_STRING: &str = "\x1b[32m";
const SYNTAX_NUMBER: &str = "\x1b[36m";
const SYNTAX_COMMENT: &str = "\x1b[90m";

/// How many display columns a tab expands to -- rustc uses 4 as well; the
/// expansion is what keeps underline carets aligned under tabbed source.
const TAB_WIDTH: usize = 4;

/// A multi-line label spanning more than this many lines elides its middle
/// with a `...` gutter row -- nobody needs 200 quoted lines to see where a
/// brace opened and where it failed to close.
const MAX_MULTILINE_LINES: usize = 5;

pub struct Renderer {
    colors: bool,
    highlighter: Option<Box<dyn Highlighter>>,
}

impl Renderer {
    pub fn new(colors: bool) -> Self {
        Self { colors, highlighter: None }
    }

    pub fn with_highlighter(mut self, highlighter: Box<dyn Highlighter>) -> Self {
        self.highlighter = Some(highlighter);
        self
    }

    /// Renders one diagnostic to a string (no trailing newline). `file` is
    /// the source every label span indexes into; pass `None` for a
    /// file-less diagnostic (e.g. a module-resolution failure with no
    /// meaningful span) -- labels are then skipped and only the headline
    /// and footers render.
    pub fn render(&self, diagnostic: &Diagnostic, file: Option<&SourceFile>) -> String {
        let mut out = String::new();
        self.render_header(&mut out, diagnostic);

        let mut width = 0;
        if let Some(file) = file
            && !diagnostic.labels.is_empty()
        {
            width = self.render_snippet(&mut out, diagnostic, file);
            if !diagnostic.notes.is_empty() || !diagnostic.helps.is_empty() {
                out.push('\n');
                self.push_empty_gutter(&mut out, width);
            }
        }

        for note in &diagnostic.notes {
            out.push('\n');
            self.render_footer(&mut out, width, "note", note);
        }
        for help in &diagnostic.helps {
            out.push('\n');
            self.render_footer(&mut out, width, "help", help);
        }
        out
    }

    fn paint(&self, code: &str, text: &str) -> String {
        if self.colors && !text.is_empty() { format!("{code}{text}{RESET}") } else { text.to_string() }
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

    fn render_header(&self, out: &mut String, d: &Diagnostic) {
        out.push_str(&self.paint(self.severity_color(d.severity), d.severity.name()));
        out.push_str(&self.paint(BOLD, &format!(": {}", d.message)));
    }

    /// The `-->` header plus the annotated snippet block. Returns the
    /// gutter width so the footers can align under it.
    fn render_snippet(&self, out: &mut String, d: &Diagnostic, file: &SourceFile) -> usize {
        let mut labels: Vec<&Label> = d.labels.iter().collect();
        labels.sort_by_key(|l| (l.span.start, l.span.end));

        let last_line = |l: &Label| file.line_of(l.span.end.saturating_sub(1).max(l.span.start));
        let width = labels.iter().map(|l| digits(last_line(l))).max().unwrap_or(1);
        // Every source line gets a 2-column bar area after the gutter when
        // any label is multi-line, so `|` continuation bars have somewhere
        // to live without shifting text between lines.
        let pad = if labels.iter().any(|l| last_line(l) > file.line_of(l.span.start)) { 2 } else { 0 };

        let primary = d.primary_label().expect("render_snippet is only called with labels present");
        let (loc_line, loc_col) = file.line_col(primary.span.start);
        out.push('\n');
        out.push_str(&" ".repeat(width));
        out.push_str(&self.paint(BLUE, "--> "));
        out.push_str(&format!("{}:{}:{}", file.name(), loc_line, loc_col));

        out.push('\n');
        self.push_empty_gutter(out, width);

        let highlights = match (&self.highlighter, self.colors) {
            (Some(h), true) => h.highlight(file.source()),
            _ => Vec::new(),
        };
        let ctx = SnippetCtx { file, width, pad, highlights };

        let mut last_printed: Option<usize> = None;
        for label in labels {
            let start_line = file.line_of(label.span.start);
            let end_line = last_line(label);
            self.render_gap(out, &ctx, last_printed, start_line);
            if start_line == end_line {
                if last_printed != Some(start_line) {
                    out.push('\n');
                    self.render_source_line(out, &ctx, start_line, "  ");
                }
                self.render_single_underline(out, &ctx, label, d.severity, start_line);
            } else {
                self.render_multiline_label(out, &ctx, label, d.severity, start_line, end_line);
            }
            last_printed = Some(end_line);
        }
        width
    }

    /// A `...` gutter row when the next printed line isn't adjacent to the
    /// last one; the intervening line itself when the gap is exactly one
    /// (showing it beats eliding it).
    fn render_gap(&self, out: &mut String, ctx: &SnippetCtx, last: Option<usize>, next: usize) {
        let Some(last) = last else { return };
        if next == last + 2 {
            out.push('\n');
            self.render_source_line(out, ctx, last + 1, "  ");
        } else if next > last + 2 {
            out.push('\n');
            out.push_str(&self.paint(BLUE, "..."));
        }
    }

    /// `12 |   text` -- one syntax-highlighted source line. `bar` fills the
    /// 2-column bar area (`"| "` inside an open multi-line span, `"  "`
    /// otherwise) and is skipped entirely when the snippet has no
    /// multi-line labels.
    fn render_source_line(&self, out: &mut String, ctx: &SnippetCtx, line: usize, bar: &str) {
        out.push_str(&self.paint(BLUE, &format!("{:>width$} | ", line, width = ctx.width)));
        if ctx.pad > 0 {
            out.push_str(bar);
        }
        out.push_str(&self.highlighted_line(ctx, line));
    }

    /// `   |    ^^^^ message` under the given line.
    fn render_single_underline(&self, out: &mut String, ctx: &SnippetCtx, label: &Label, severity: Severity, line: usize) {
        let text = ctx.file.line_text(line);
        let line_start = ctx.file.line_start(line);
        let start = label.span.start.saturating_sub(line_start).min(text.len());
        let end = label.span.end.saturating_sub(line_start).min(text.len());
        let disp_start = display_col(text, start);
        let marker_width = (display_col(text, end) - disp_start).max(1);

        let marker = if label.style == LabelStyle::Primary { "^" } else { "-" };
        let mut row = String::new();
        row.push_str(&" ".repeat(ctx.pad + disp_start));
        row.push_str(&marker.repeat(marker_width));
        if !label.message.is_empty() {
            row.push(' ');
            row.push_str(&label.message);
        }

        out.push('\n');
        out.push_str(&self.paint(BLUE, &format!("{:>width$} | ", "", width = ctx.width)));
        out.push_str(&self.paint(self.label_color(severity, label.style), &row));
    }

    /// The bracket form: start line, ` ___^` opening row, barred body lines
    /// (middle elided past `MAX_MULTILINE_LINES`), and a `|___^ message`
    /// closing row.
    fn render_multiline_label(
        &self,
        out: &mut String,
        ctx: &SnippetCtx,
        label: &Label,
        severity: Severity,
        start_line: usize,
        end_line: usize,
    ) {
        let color = self.label_color(severity, label.style);
        let marker = if label.style == LabelStyle::Primary { "^" } else { "-" };

        out.push('\n');
        self.render_source_line(out, ctx, start_line, "  ");

        // ` ____^` -- caret under the span's first character, which sits 2
        // bar-area columns right of where its display column says.
        let start_text = ctx.file.line_text(start_line);
        let start_byte = label.span.start.saturating_sub(ctx.file.line_start(start_line)).min(start_text.len());
        let caret_at = 2 + display_col(start_text, start_byte);
        out.push('\n');
        out.push_str(&self.paint(BLUE, &format!("{:>width$} | ", "", width = ctx.width)));
        out.push_str(&self.paint(color, &format!(" {}{marker}", "_".repeat(caret_at.saturating_sub(1)))));

        let body: Vec<usize> = if end_line - start_line > MAX_MULTILINE_LINES {
            // First body line, elision marker (0 = sentinel), last two lines.
            vec![start_line + 1, 0, end_line - 1, end_line]
        } else {
            (start_line + 1..=end_line).collect()
        };
        for line in body {
            out.push('\n');
            if line == 0 {
                out.push_str(&self.paint(BLUE, &format!("{:<width$}", "...", width = ctx.width + 3)));
                out.push_str(&self.paint(color, "|"));
            } else {
                self.render_source_line_with_open_bar(out, ctx, line, color);
            }
        }

        // `|___^ message` -- caret under the span's last character.
        let end_text = ctx.file.line_text(end_line);
        let end_byte = label.span.end.saturating_sub(ctx.file.line_start(end_line)).min(end_text.len());
        let caret_at = 2 + display_col(end_text, end_byte).saturating_sub(1);
        let mut row = format!("|{}{marker}", "_".repeat(caret_at.saturating_sub(1)));
        if !label.message.is_empty() {
            row.push(' ');
            row.push_str(&label.message);
        }
        out.push('\n');
        out.push_str(&self.paint(BLUE, &format!("{:>width$} | ", "", width = ctx.width)));
        out.push_str(&self.paint(color, &row));
    }

    /// Like `render_source_line`, but the bar area's `|` is the label's own
    /// continuation bar, painted in the label's color rather than gutter
    /// blue.
    fn render_source_line_with_open_bar(&self, out: &mut String, ctx: &SnippetCtx, line: usize, color: &str) {
        out.push_str(&self.paint(BLUE, &format!("{:>width$} | ", line, width = ctx.width)));
        out.push_str(&self.paint(color, "|"));
        out.push(' ');
        out.push_str(&self.highlighted_line(ctx, line));
    }

    fn push_empty_gutter(&self, out: &mut String, width: usize) {
        out.push_str(&self.paint(BLUE, &format!("{:>width$} |", "", width = width)));
    }

    /// `   = note: text`, continuation lines aligned under the text.
    /// `width == 0` means no snippet was rendered, so there's no gutter to
    /// align under.
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

    /// One line's text, tab-expanded, with syntax-class coloring applied
    /// per character run (colors precomputed once per snippet in
    /// `SnippetCtx::highlights`).
    fn highlighted_line(&self, ctx: &SnippetCtx, line: usize) -> String {
        let text = ctx.file.line_text(line);
        if ctx.highlights.is_empty() {
            return expand_tabs(text);
        }
        let line_start = ctx.file.line_start(line);
        let relevant = line_highlights(&ctx.highlights, line_start, line_start + text.len());

        let mut out = String::new();
        let mut current: Option<TokenClass> = None;
        for (i, ch) in text.char_indices() {
            let offset = line_start + i;
            let class = relevant
                .iter()
                .find(|(span, _)| span.start <= offset && offset < span.end)
                .map(|&(_, class)| class);
            if class != current {
                if current.is_some() {
                    out.push_str(RESET);
                }
                if let Some(class) = class {
                    out.push_str(class_color(class));
                }
                current = class;
            }
            match ch {
                '\t' => out.push_str(&" ".repeat(TAB_WIDTH)),
                _ => out.push(ch),
            }
        }
        if current.is_some() {
            out.push_str(RESET);
        }
        out
    }
}

struct SnippetCtx<'a> {
    file: &'a SourceFile,
    width: usize,
    /// Extra columns between the gutter and source text: 2 when any label
    /// is multi-line (room for continuation bars), else 0.
    pad: usize,
    highlights: Vec<(Span, TokenClass)>,
}

fn class_color(class: TokenClass) -> &'static str {
    match class {
        TokenClass::Keyword => SYNTAX_KEYWORD,
        TokenClass::String => SYNTAX_STRING,
        TokenClass::Number => SYNTAX_NUMBER,
        TokenClass::Comment => SYNTAX_COMMENT,
    }
}

fn digits(n: usize) -> usize {
    n.max(1).ilog10() as usize + 1
}

fn expand_tabs(text: &str) -> String {
    text.replace('\t', &" ".repeat(TAB_WIDTH))
}

/// 0-based display column for a byte offset within one line's text --
/// counts chars, with tabs expanded, so carets line up under what the
/// terminal actually shows. An offset past the line's end just keeps
/// counting past the last character (a zero-width "just past EOF" span
/// lands one column after the text).
fn display_col(text: &str, byte_offset: usize) -> usize {
    let mut col = 0;
    for (i, ch) in text.char_indices() {
        if i >= byte_offset {
            return col;
        }
        col += if ch == '\t' { TAB_WIDTH } else { 1 };
    }
    col + byte_offset.saturating_sub(text.len())
}

/// The sorted `highlights` entries overlapping `[line_start, line_end)`.
fn line_highlights(highlights: &[(Span, TokenClass)], line_start: usize, line_end: usize) -> &[(Span, TokenClass)] {
    let begin = highlights.partition_point(|(span, _)| span.end <= line_start);
    let count = highlights[begin..].partition_point(|(span, _)| span.start < line_end);
    &highlights[begin..begin + count]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostic::Diagnostic;

    fn render_plain(d: &Diagnostic, source: &str) -> String {
        Renderer::new(false).render(d, Some(&SourceFile::new("test.omg", source)))
    }

    #[test]
    fn single_line_error() {
        let source = "a := 1;\nb = ;\n";
        let d = Diagnostic::error("expected an expression, found ';'")
            .with_label(Span::new(12, 13), "expected an expression");
        assert_eq!(
            render_plain(&d, source),
            "\
error: expected an expression, found ';'
 --> test.omg:2:5
  |
2 | b = ;
  |     ^ expected an expression"
        );
    }

    #[test]
    fn secondary_label_and_footers() {
        let source = "x : i32;\nx : u8;\n";
        let d = Diagnostic::error("`x` is already declared in this scope")
            .with_label(Span::new(9, 10), "redeclared here")
            .with_secondary_label(Span::new(0, 1), "first declared here")
            .with_note("shadowing is only allowed across scopes")
            .with_help("give the second declaration a different name");
        assert_eq!(
            render_plain(&d, source),
            "\
error: `x` is already declared in this scope
 --> test.omg:2:1
  |
1 | x : i32;
  | - first declared here
2 | x : u8;
  | ^ redeclared here
  |
  = note: shadowing is only allowed across scopes
  = help: give the second declaration a different name"
        );
    }

    #[test]
    fn multiline_label() {
        let source = "v := if x {\n    1\n} else {\n    \"s\"\n};\n";
        let d = Diagnostic::error("mismatched branch types").with_label(Span::new(5, 36), "branches disagree");
        assert_eq!(
            render_plain(&d, source),
            "\
error: mismatched branch types
 --> test.omg:1:6
  |
1 |   v := if x {
  |  ______^
2 | |     1
3 | | } else {
4 | |     \"s\"
5 | | };
  | |_^ branches disagree"
        );
    }

    #[test]
    fn zero_width_span_at_eof() {
        let source = "main() => i32 {";
        let d = Diagnostic::error("expected '}', found end of input").with_label(Span::new(15, 15), "expected '}'");
        assert_eq!(
            render_plain(&d, source),
            "\
error: expected '}', found end of input
 --> test.omg:1:16
  |
1 | main() => i32 {
  |                ^ expected '}'"
        );
    }

    #[test]
    fn labels_far_apart_get_elision_row() {
        let source = "l1;\nl2;\nl3;\nl4;\nl5;\nl6;\n";
        let d = Diagnostic::error("two spots")
            .with_label(Span::new(0, 2), "here")
            .with_secondary_label(Span::new(20, 22), "and here");
        assert_eq!(
            render_plain(&d, source),
            "\
error: two spots
 --> test.omg:1:1
  |
1 | l1;
  | ^^ here
...
6 | l6;
  | -- and here"
        );
    }

    #[test]
    fn no_labels_renders_headline_and_footers_only() {
        let d = Diagnostic::error("no such module 'foo'").with_help("expected foo.omg or foo/ in a search root");
        assert_eq!(
            Renderer::new(false).render(&d, None),
            "\
error: no such module 'foo'
= help: expected foo.omg or foo/ in a search root"
        );
    }

    #[test]
    fn long_multiline_elides_middle() {
        let source = (1..=12).map(|i| format!("line{i};\n")).collect::<String>();
        let end = source.len() - 1; // last ';'
        let d = Diagnostic::error("big span").with_label(Span::new(0, end), "all of it");
        let rendered = render_plain(&d, &source);
        assert!(rendered.contains("..."), "expected elision row:\n{rendered}");
        assert!(!rendered.contains("line6"), "middle lines should be elided:\n{rendered}");
        assert!(rendered.contains("line12"), "last line must render:\n{rendered}");
    }
}
