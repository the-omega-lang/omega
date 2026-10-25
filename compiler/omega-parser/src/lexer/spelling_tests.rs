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
            assert!(
                token
                    .spelling
                    .chars()
                    .all(|ch| ch.is_ascii_alphabetic() || ch == '_')
            );
        }
    }
}

fn kinds(source: &str) -> Vec<TokenKind> {
    let lexed = lex(source);
    assert!(lexed.errors.is_empty(), "`{source}`: {:?}", lexed.errors);
    lexed
        .tokens
        .into_iter()
        .map(|token| token.kind)
        .filter(|kind| !matches!(kind, TokenKind::Eof))
        .collect()
}

fn ident(name: &str) -> TokenKind {
    TokenKind::Ident(name.to_string())
}

#[test]
fn dereference_is_one_token_ahead_of_compound_assignment() {
    use TokenKind::*;
    assert_eq!(kinds("p.*=v"), [ident("p"), DotStar, Eq, ident("v")]);
    assert_eq!(kinds("p.**=v"), [ident("p"), DotStar, StarEq, ident("v")]);
    assert_eq!(kinds("x.*.*"), [ident("x"), DotStar, DotStar]);
}

#[test]
fn ranges_keep_their_dots_before_a_star() {
    use TokenKind::*;
    assert_eq!(kinds("a...*b"), [ident("a"), DotDotDot, Star, ident("b")]);
    assert_eq!(kinds("a..*b"), [ident("a"), DotDot, Star, ident("b")]);
}
