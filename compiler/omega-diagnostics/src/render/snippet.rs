use super::{BLUE, RESET, Renderer};
use crate::diagnostic::{Diagnostic, Label, LabelStyle, Severity};
use crate::highlight::TokenClass;
use crate::source::{SourceFile, SourceId, SourceRegistry, TAB_WIDTH, display_column};
use crate::span::Span;

const MAX_MULTILINE_LINES: usize = 5;
const SYNTAX_KEYWORD: &str = "\x1b[35m";
const SYNTAX_STRING: &str = "\x1b[32m";
const SYNTAX_NUMBER: &str = "\x1b[36m";
const SYNTAX_COMMENT: &str = "\x1b[90m";

impl Renderer {
    /// Renders one `-->` section per source file the diagnostic labels, the
    /// primary label's file first. Returns the shared gutter width, or 0 when
    /// no label resolved to retained source.
    pub(super) fn render_snippets(
        &self,
        out: &mut String,
        d: &Diagnostic,
        sources: &SourceRegistry,
    ) -> usize {
        let resolved = resolve_labels(d, sources);
        if resolved.is_empty() {
            return 0;
        }

        // Gutter width and the multi-line bar area are computed across every
        // section so source columns line up between files.
        let width = resolved
            .iter()
            .map(|(_, file, label)| digits(last_line(file, label)))
            .max()
            .unwrap_or(1);
        let pad = if resolved
            .iter()
            .any(|(_, file, label)| last_line(file, label) > file.line_of(label.span.start))
        {
            2
        } else {
            0
        };

        for (index, section) in sections(&resolved).into_iter().enumerate() {
            if index > 0 {
                out.push('\n');
                self.push_empty_gutter(out, width);
            }
            self.render_section(out, d, section.file, &section.labels, width, pad);
        }
        width
    }

    fn render_section(
        &self,
        out: &mut String,
        d: &Diagnostic,
        file: &SourceFile,
        labels: &[&Label],
        width: usize,
        pad: usize,
    ) {
        let mut labels = labels.to_vec();
        labels.sort_by_key(|l| (l.span.start, l.span.end));

        let anchor = labels
            .iter()
            .find(|l| l.style == LabelStyle::Primary)
            .or_else(|| labels.first())
            .expect("a section is only built from at least one label");
        let (loc_line, loc_col) = file.line_col(anchor.span.start);
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
            let end_line = last_line(file, label);
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

    fn highlighted_line(&self, ctx: &SnippetCtx, line: usize) -> String {
        let text = ctx.file.line_text(line);
        if ctx.highlights.is_empty() {
            return expand_tabs(text);
        }
        let line_start = ctx.file.line_start(line);
        let relevant = line_highlights(&ctx.highlights, line_start, line_start + text.len());

        let mut out = String::new();
        let mut current: Option<TokenClass> = None;
        let mut highlight_index = 0;
        for (i, ch) in text.char_indices() {
            let offset = line_start + i;
            while relevant
                .get(highlight_index)
                .is_some_and(|(span, _)| span.end <= offset)
            {
                highlight_index += 1;
            }
            let class = relevant
                .get(highlight_index)
                .filter(|(span, _)| span.start <= offset && offset < span.end)
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

type ResolvedLabel<'a> = (SourceId, &'a SourceFile, &'a Label);

struct Section<'a> {
    file: &'a SourceFile,
    labels: Vec<&'a Label>,
}

/// Labels paired with the file they index. A label whose source is unknown to
/// the registry is dropped rather than rendered against another file.
fn resolve_labels<'a>(d: &'a Diagnostic, sources: &'a SourceRegistry) -> Vec<ResolvedLabel<'a>> {
    d.labels
        .iter()
        .filter_map(|label| {
            let id = label.source.or(d.source)?;
            Some((id, sources.get(id)?, label))
        })
        .collect()
}

/// One section per source, the primary label's source first and the rest in
/// first-label order.
fn sections<'a>(resolved: &[ResolvedLabel<'a>]) -> Vec<Section<'a>> {
    let primary = resolved
        .iter()
        .find(|(_, _, label)| label.style == LabelStyle::Primary)
        .or_else(|| resolved.first())
        .map(|&(id, _, _)| id);

    let mut order: Vec<SourceId> = primary.into_iter().collect();
    for &(id, _, _) in resolved {
        if !order.contains(&id) {
            order.push(id);
        }
    }

    order
        .into_iter()
        .map(|id| Section {
            file: resolved
                .iter()
                .find(|(candidate, _, _)| *candidate == id)
                .map(|&(_, file, _)| file)
                .expect("section ids come from the resolved labels"),
            labels: resolved
                .iter()
                .filter(|(candidate, _, _)| *candidate == id)
                .map(|&(_, _, label)| label)
                .collect(),
        })
        .collect()
}

fn last_line(file: &SourceFile, label: &Label) -> usize {
    file.line_of(label.span.end.saturating_sub(1).max(label.span.start))
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
    (display_column(text, start), display_column(text, end))
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

fn line_highlights(
    highlights: &[(Span, TokenClass)],
    line_start: usize,
    line_end: usize,
) -> &[(Span, TokenClass)] {
    let begin = highlights.partition_point(|(span, _)| span.end <= line_start);
    let count = highlights[begin..].partition_point(|(span, _)| span.start < line_end);
    &highlights[begin..begin + count]
}
