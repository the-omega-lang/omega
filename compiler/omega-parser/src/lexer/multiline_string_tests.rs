use super::{TokenKind, lex};
use crate::diagnostics::ParseErrorKind;

fn single_str_token(source: &str) -> (TokenKind, Vec<ParseErrorKind>) {
    let lexed = lex(source);
    let errors: Vec<ParseErrorKind> = lexed.errors.into_iter().map(|e| e.kind).collect();
    assert_eq!(
        lexed.tokens.len(),
        2,
        "expected exactly one Str token then Eof, got {:?}",
        lexed.tokens
    );
    (lexed.tokens[0].kind.clone(), errors)
}

#[test]
fn ordinary_string_unaffected() {
    let (kind, errors) = single_str_token(r#""hello""#);
    assert_eq!(kind, TokenKind::Str("hello".to_string()));
    assert!(errors.is_empty());
}

#[test]
fn empty_string_unaffected() {
    // A bare `""` must never be swept into the multi-line path.
    let (kind, errors) = single_str_token(r#""""#);
    assert_eq!(kind, TokenKind::Str(String::new()));
    assert!(errors.is_empty());
}

#[test]
fn three_quote_multiline_closes_on_matching_run() {
    let (kind, errors) = single_str_token("\"\"\"hello\"\"\"");
    assert_eq!(kind, TokenKind::Str("hello".to_string()));
    assert!(errors.is_empty());
}

#[test]
fn mismatched_inner_run_is_literal_content() {
    // A run of 2 quotes inside a 3-quote-delimited string doesn't
    // terminate it -- straight from the user's own worked example.
    let (kind, errors) = single_str_token("\"\"\"a (\"\") b\"\"\"");
    assert_eq!(kind, TokenKind::Str("a (\"\") b".to_string()));
    assert!(errors.is_empty());
}

#[test]
fn nine_quote_multiline_with_seven_quote_run_inside() {
    // The user's second worked example: opening with 9 quotes, a run
    // of 7 inside must not terminate it.
    let source = "\"\"\"\"\"\"\"\"\"middle \"\"\"\"\"\"\" end\"\"\"\"\"\"\"\"\"";
    let (kind, errors) = single_str_token(source);
    assert_eq!(
        kind,
        TokenKind::Str("middle \"\"\"\"\"\"\" end".to_string())
    );
    assert!(errors.is_empty());
}

#[test]
fn even_count_delimiter_is_a_dedicated_error_but_still_produces_a_token() {
    let (kind, errors) = single_str_token("\"\"\"\"content\"\"\"\"");
    assert_eq!(kind, TokenKind::Str("content".to_string()));
    assert_eq!(
        errors,
        vec![ParseErrorKind::EvenMultilineStringDelimiter { count: 4 }]
    );
}

#[test]
fn unterminated_multiline_string_errors() {
    let lexed = lex("\"\"\"never closes");
    assert!(matches!(lexed.tokens[0].kind, TokenKind::Eof) || lexed.tokens.len() == 1);
    assert!(matches!(
        lexed.errors.last().map(|e| &e.kind),
        Some(ParseErrorKind::UnterminatedString)
    ));
}

#[test]
fn byte_string_multiline_works_identically() {
    let lexed = lex("b\"\"\"hello\"\"\"");
    assert_eq!(
        lexed.tokens[0].kind,
        TokenKind::ByteStr("hello".to_string())
    );
    assert!(lexed.errors.is_empty());
}
