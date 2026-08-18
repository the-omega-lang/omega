
use crate::diagnostic::{Diagnostic, Footer, Label, LabelStyle, Severity};
use crate::highlight::{Highlighter, TokenClass};
use crate::source::SourceFile;
use crate::span::Span;

pub const RESET: &str = "\x1b[0m";
pub const BOLD: &str = "\x1b[1m";
pub const RED: &str = "\x1b[1;31m";
pub const YELLOW: &str = "\x1b[1;33m";
pub const GREEN: &str = "\x1b[1;32m";
pub const BLUE: &str = "\x1b[1;34m";
pub const CYAN: &str = "\x1b[1;36m";

const SYNTAX_KEYWORD: &str = "\x1b[35m";
const SYNTAX_STRING: &str = "\x1b[32m";
const SYNTAX_NUMBER: &str = "\x1b[36m";
const SYNTAX_COMMENT: &str = "\x1b[90m";

pub fn paint(colors: bool, code: &str, text: &str) -> String {
    if colors && !text.is_empty() {
        format!("{code}{text}{RESET}")
    } else {
        text.to_string()
    }
}

const TAB_WIDTH: usize = 4;

const MAX_MULTILINE_LINES: usize = 5;

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

    fn render_header(&self, out: &mut String, d: &Diagnostic) {
        out.push_str(&self.paint(self.severity_color(d.severity), d.severity.name()));
        out.push_str(&self.paint(BOLD, &format!(": {}", d.message)));
    }

    fn render_snippet(&self, out: &mut String, d: &Diagnostic, file: &SourceFile) -> usize {
        let mut labels: Vec<&Label> = d.labels.iter().collect();
        labels.sort_by_key(|l| (l.span.start, l.span.end));

        let last_line = |l: &Label| file.line_of(l.span.end.saturating_sub(1).max(l.span.start));
        let width = labels
            .iter()
            .map(|l| digits(last_line(l)))
            .max()
            .unwrap_or(1);
        // Every source line gets a 2-column bar area after the gutter when
        // any label is multi-line, so `|` continuation bars have somewhere
        // to live without shifting text between lines.
        let pad = if labels
            .iter()
            .any(|l| last_line(l) > file.line_of(l.span.start))
        {
            2
        } else {
            0
        };

        let primary = d
            .primary_label()
            .expect("render_snippet is only called with labels present");
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
        let ctx = SnippetCtx {
            file,
            width,
            pad,
            highlights,
        };

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

    fn render_source_line(&self, out: &mut String, ctx: &SnippetCtx, line: usize, bar: &str) {
        out.push_str(&self.paint(BLUE, &format!("{:>width$} | ", line, width = ctx.width)));
        if ctx.pad > 0 {
            out.push_str(bar);
        }
        out.push_str(&self.highlighted_line(ctx, line));
    }

    fn render_single_underline(
        &self,
        out: &mut String,
        ctx: &SnippetCtx,
        label: &Label,
        severity: Severity,
        line: usize,
    ) {
        let (disp_start, disp_end) = label_columns(ctx.file, label.span, line);
        let marker_width = (disp_end - disp_start).max(1);
        let marker = if label.style == LabelStyle::Primary {
            "^"
        } else {
            "-"
        };
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
        let marker = if label.style == LabelStyle::Primary {
            "^"
        } else {
            "-"
        };

        out.push('\n');
        self.render_source_line(out, ctx, start_line, "  ");

        // ` ____^` -- caret under the span's first character, which sits 2
        // bar-area columns right of where its display column says.
        let (start_col, _) = label_columns(ctx.file, label.span, start_line);
        let caret_at = 2 + start_col;
        out.push('\n');
        out.push_str(&self.paint(BLUE, &format!("{:>width$} | ", "", width = ctx.width)));
        out.push_str(&self.paint(
            color,
            &format!(" {}{marker}", "_".repeat(caret_at.saturating_sub(1))),
        ));

        let body: Vec<BodyRow> = if end_line - start_line > MAX_MULTILINE_LINES {
            vec![
                BodyRow::Source(start_line + 1),
                BodyRow::Elision,
                BodyRow::Source(end_line - 1),
                BodyRow::Source(end_line),
            ]
        } else {
            (start_line + 1..=end_line).map(BodyRow::Source).collect()
        };
        for row in body {
            out.push('\n');
            match row {
                BodyRow::Elision => {
                    out.push_str(
                        &self.paint(BLUE, &format!("{:<width$}", "...", width = ctx.width + 3)),
                    );
                    out.push_str(&self.paint(color, "|"));
                }
                BodyRow::Source(line) => {
                    self.render_source_line_with_open_bar(out, ctx, line, color)
                }
            }
        }
        // `|___^ message` -- caret under the span's last character.
        let (_, end_col) = label_columns(ctx.file, label.span, end_line);
        let caret_at = 2 + end_col.saturating_sub(1);
        let mut row = format!("|{}{marker}", "_".repeat(caret_at.saturating_sub(1)));
        if !label.message.is_empty() {
            row.push(' ');
            row.push_str(&label.message);
        }
        out.push('\n');
        out.push_str(&self.paint(BLUE, &format!("{:>width$} | ", "", width = ctx.width)));
        out.push_str(&self.paint(color, &row));
    }

    fn render_source_line_with_open_bar(
        &self,
        out: &mut String,
        ctx: &SnippetCtx,
        line: usize,
        color: &str,
    ) {
        out.push_str(&self.paint(BLUE, &format!("{:>width$} | ", line, width = ctx.width)));
        out.push_str(&self.paint(color, "|"));
        out.push(' ');
        out.push_str(&self.highlighted_line(ctx, line));
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
    pad: usize,
    highlights: Vec<(Span, TokenClass)>,
}

enum BodyRow {
    Source(usize),
    Elision,
}

fn label_columns(file: &SourceFile, span: Span, line: usize) -> (usize, usize) {
    let text = file.line_text(line);
    let line_start = file.line_start(line);
    let start = span.start.saturating_sub(line_start).min(text.len());
    let end = span.end.saturating_sub(line_start).min(text.len());
    (display_col(text, start), display_col(text, end))
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

fn line_highlights(
    highlights: &[(Span, TokenClass)],
    line_start: usize,
    line_end: usize,
) -> &[(Span, TokenClass)] {
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
        let d = Diagnostic::error("mismatched branch types")
            .with_label(Span::new(5, 36), "branches disagree");
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
        let d = Diagnostic::error("expected '}', found end of input")
            .with_label(Span::new(15, 15), "expected '}'");
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
        let d = Diagnostic::error("no such module 'foo'")
            .with_help("expected foo.omg or foo/ in a search root");
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
        assert!(
            rendered.contains("..."),
            "expected elision row:\n{rendered}"
        );
        assert!(
            !rendered.contains("line6"),
            "middle lines should be elided:\n{rendered}"
        );
        assert!(
            rendered.contains("line12"),
            "last line must render:\n{rendered}"
        );
    }
    #[test]
    fn footer_order_matches_constructor_order() {
        let rendered = Renderer::new(false).render(
            &Diagnostic::error("broken")
                .with_help("fix it")
                .with_note("context"),
            None,
        );
        assert!(rendered.find("= help: fix it").unwrap() < rendered.find("= note: context").unwrap());
    }

    #[test]
    fn same_line_labels_render_one_source_row_and_two_underlines() {
        let source = "left right\n";
        let rendered = render_plain(
            &Diagnostic::error("two labels")
                .with_label(Span::new(0, 4), "left")
                .with_secondary_label(Span::new(5, 10), "right"),
            source,
        );
        assert_eq!(rendered.matches("1 | left right").count(), 1);
        assert_eq!(rendered.matches(" | ").count(), 3);
    }
}
