use super::*;
use crate::diagnostic::Diagnostic;
use crate::span::Span;

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

#[test]
fn tabbed_primary_column_matches_expanded_source() {
    let source = "\tbad\n";
    let rendered = render_plain(
        &Diagnostic::error("bad token").with_label(Span::new(1, 4), "here"),
        source,
    );
    assert!(rendered.contains("--> test.omg:1:5"), "{rendered}");
    assert!(rendered.contains("1 |     bad"), "{rendered}");
}
