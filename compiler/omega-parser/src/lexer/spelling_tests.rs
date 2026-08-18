use super::{FIXED_TOKENS, FixedTokenClass, TokenKind, lex};

fn sole_token(source: &str) -> TokenKind {
    let lexed = lex(source);
    assert!(
        lexed.errors.is_empty(),
        "`{source}` must lex cleanly, got {:?}",
        lexed.errors
    );
    let kinds: Vec<&TokenKind> = lexed
        .tokens
        .iter()
        .map(|token| &token.kind)
        .filter(|kind| !matches!(kind, TokenKind::Eof))
        .collect();
    assert_eq!(
        kinds.len(),
        1,
        "`{source}` must lex as exactly one token, got {kinds:?}"
    );
    kinds[0].clone()
}

#[test]
fn fixed_token_registry_drives_lexing_spelling_and_descriptions() {
    for token in FIXED_TOKENS {
        let kind = sole_token(token.spelling);
        assert_eq!(&kind, &token.kind, "`{}` lexed as {kind:?}", token.spelling);
        assert_eq!(kind.spelling(), Some(token.spelling));
        assert_eq!(kind.describe(), format!("'{}'", token.spelling));

        if token.class == FixedTokenClass::Keyword {
            assert!(token.spelling.chars().all(|ch| ch.is_ascii_alphabetic()));
        }
    }
}
