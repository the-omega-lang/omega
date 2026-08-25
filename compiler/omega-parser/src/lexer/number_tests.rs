use super::{TokenKind, lex};
use crate::ast::expression::NumberExpr;
use crate::diagnostics::ParseErrorKind;

fn number(source: &str) -> NumberExpr {
    let lexed = lex(source);
    assert!(
        lexed.errors.is_empty(),
        "`{source}` must lex cleanly, got {:?}",
        lexed.errors
    );
    match &lexed.tokens[0].kind {
        TokenKind::Number(number) => number.clone(),
        other => panic!("`{source}` lexed as {other:?}"),
    }
}

fn suffix_error(source: &str) -> (String, String) {
    let lexed = lex(source);
    match lexed.errors.as_slice() {
        [error] => match &error.kind {
            ParseErrorKind::NumberSuffixNeedsSeparator { suffix, suggestion } => {
                (suffix.0.clone(), suggestion.clone())
            }
            other => panic!("`{source}` produced {other:?}"),
        },
        errors => panic!("`{source}` produced {errors:?}"),
    }
}

#[test]
fn hexadecimal_suffixes_require_an_underscore_separator() {
    assert_eq!(
        suffix_error("0xdeadbeefusize"),
        ("usize".to_string(), "0xdeadbeef_usize".to_string())
    );
    assert_eq!(
        suffix_error("0xFFu8"),
        ("u8".to_string(), "0xFF_u8".to_string())
    );
    assert_eq!(
        suffix_error("0xFF_FFi64"),
        ("i64".to_string(), "0xFF_FF_i64".to_string())
    );
}

#[test]
fn separated_hexadecimal_suffixes_lex_cleanly() {
    let literal = number("0xdeadbeef_usize");
    assert_eq!(literal.integer_part, "deadbeef");
    assert_eq!(
        literal.explicit_type.as_ref().map(|t| t.0.as_str()),
        Some("usize")
    );
}

#[test]
fn bases_without_alphabetic_digits_keep_attached_suffixes() {
    for (source, digits, suffix) in [
        ("123u64", "123", "u64"),
        ("0b1010u8", "1010", "u8"),
        ("0o755isize", "755", "isize"),
        ("42i32", "42", "i32"),
    ] {
        let literal = number(source);
        assert_eq!(literal.integer_part, digits, "`{source}`");
        assert_eq!(
            literal.explicit_type.as_ref().map(|t| t.0.as_str()),
            Some(suffix),
            "`{source}`"
        );
    }
    assert_eq!(number("3.5f32").fractional_part.as_deref(), Some("5"));
}

#[test]
fn unsuffixed_hexadecimal_literals_are_unaffected() {
    let literal = number("0xdeadbeef");
    assert_eq!(literal.integer_part, "deadbeef");
    assert!(literal.explicit_type.is_none());
}
