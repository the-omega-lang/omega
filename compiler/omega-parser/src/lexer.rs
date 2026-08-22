use crate::ast::expression::{NumberBase, NumberExpr};
use crate::ast::identifier::Ident;
use crate::ast::identifier::Origin;
use crate::diagnostics::{ParseError, ParseErrorKind, Span};

#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    Ident(String),
    Number(NumberExpr),
    Str(String),
    ByteStr(String),
    Char(char),
    Metavar(String),
    /// Raw backend assembly text captured verbatim between the braces of an
    /// `asm(...) => { ... }` statement. Never produced by ordinary tokenization.
    AsmBody(String),

    // Contextual words stay `Ident` tokens so they remain usable as names.
    True,
    False,
    If,
    Else,
    Match,
    Foreign,
    Import,
    Return,
    Struct,
    Enum,
    Union,
    Spec,
    While,
    Loop,
    For,
    Break,
    Continue,
    Defer,
    Macro,

    // Multi-char punctuation, maximal-munch (tried longest-first during
    // lexing so e.g. `...` is never mistaken for `..` followed by `.`).
    DotDotDot,
    ColonColon,
    FatArrow,
    ColonEq,
    EqEq,
    NotEq,
    LtEq,
    GtEq,
    DotDotEq,
    DotDotLt,
    DotDot,
    PlusPlus,
    MinusMinus,
    AmpAmp,
    PipePipe,
    Shl,
    Shr,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    PercentEq,
    AmpEq,
    PipeEq,
    CaretEq,
    ShlEq,
    ShrEq,

    Dollar,
    Percent,
    Amp,
    Star,
    Plus,
    Comma,
    Minus,
    Dot,
    Slash,
    Colon,
    Semi,
    Lt,
    Eq,
    Gt,
    Pipe,
    Caret,
    Tilde,
    Not,
    At,
    Question,

    // Delimiters stay flat; nesting belongs to the parser.
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,

    Eof,
}

impl TokenKind {
    pub fn spelling(&self) -> Option<&'static str> {
        FIXED_TOKENS
            .iter()
            .find(|token| &token.kind == self)
            .map(|token| token.spelling)
    }

    pub fn describe(&self) -> String {
        match self {
            Self::Ident(s) => format!("identifier '{s}'"),
            Self::Number(_) => "a number literal".to_string(),
            Self::Str(_) => "a string literal".to_string(),
            Self::ByteStr(_) => "a binary string literal".to_string(),
            Self::Char(_) => "a character literal".to_string(),
            Self::Metavar(s) => format!("'${s}'"),
            Self::AsmBody(_) => "raw assembly text".to_string(),
            Self::Eof => "end of input".to_string(),
            _ => format!("'{}'", self.spelling().expect("fixed token has a spelling")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
    pub origin: Origin,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FixedTokenClass {
    Keyword,
    Punctuation,
}

struct FixedToken {
    spelling: &'static str,
    kind: TokenKind,
    class: FixedTokenClass,
}

const fn keyword(spelling: &'static str, kind: TokenKind) -> FixedToken {
    FixedToken {
        spelling,
        kind,
        class: FixedTokenClass::Keyword,
    }
}

const fn punctuation(spelling: &'static str, kind: TokenKind) -> FixedToken {
    FixedToken {
        spelling,
        kind,
        class: FixedTokenClass::Punctuation,
    }
}

const FIXED_TOKENS: &[FixedToken] = &[
    keyword("true", TokenKind::True),
    keyword("false", TokenKind::False),
    keyword("if", TokenKind::If),
    keyword("else", TokenKind::Else),
    keyword("match", TokenKind::Match),
    keyword("foreign", TokenKind::Foreign),
    keyword("import", TokenKind::Import),
    keyword("return", TokenKind::Return),
    keyword("struct", TokenKind::Struct),
    keyword("enum", TokenKind::Enum),
    keyword("union", TokenKind::Union),
    keyword("spec", TokenKind::Spec),
    keyword("while", TokenKind::While),
    keyword("loop", TokenKind::Loop),
    keyword("for", TokenKind::For),
    keyword("break", TokenKind::Break),
    keyword("continue", TokenKind::Continue),
    keyword("defer", TokenKind::Defer),
    keyword("macro", TokenKind::Macro),
    punctuation("...", TokenKind::DotDotDot),
    punctuation("..=", TokenKind::DotDotEq),
    punctuation("..<", TokenKind::DotDotLt),
    punctuation("..", TokenKind::DotDot),
    punctuation("::", TokenKind::ColonColon),
    punctuation("=>", TokenKind::FatArrow),
    punctuation(":=", TokenKind::ColonEq),
    punctuation("==", TokenKind::EqEq),
    punctuation("!=", TokenKind::NotEq),
    punctuation("<=", TokenKind::LtEq),
    punctuation(">=", TokenKind::GtEq),
    punctuation("++", TokenKind::PlusPlus),
    punctuation("--", TokenKind::MinusMinus),
    punctuation("<<=", TokenKind::ShlEq),
    punctuation(">>=", TokenKind::ShrEq),
    punctuation("<<", TokenKind::Shl),
    punctuation(">>", TokenKind::Shr),
    punctuation("+=", TokenKind::PlusEq),
    punctuation("-=", TokenKind::MinusEq),
    punctuation("*=", TokenKind::StarEq),
    punctuation("/=", TokenKind::SlashEq),
    punctuation("%=", TokenKind::PercentEq),
    punctuation("&&", TokenKind::AmpAmp),
    punctuation("||", TokenKind::PipePipe),
    punctuation("&=", TokenKind::AmpEq),
    punctuation("|=", TokenKind::PipeEq),
    punctuation("^=", TokenKind::CaretEq),
    punctuation("$", TokenKind::Dollar),
    punctuation("%", TokenKind::Percent),
    punctuation("&", TokenKind::Amp),
    punctuation("*", TokenKind::Star),
    punctuation("+", TokenKind::Plus),
    punctuation(",", TokenKind::Comma),
    punctuation("-", TokenKind::Minus),
    punctuation(".", TokenKind::Dot),
    punctuation("/", TokenKind::Slash),
    punctuation(":", TokenKind::Colon),
    punctuation(";", TokenKind::Semi),
    punctuation("<", TokenKind::Lt),
    punctuation("=", TokenKind::Eq),
    punctuation(">", TokenKind::Gt),
    punctuation("|", TokenKind::Pipe),
    punctuation("^", TokenKind::Caret),
    punctuation("~", TokenKind::Tilde),
    punctuation("!", TokenKind::Not),
    punctuation("@", TokenKind::At),
    punctuation("?", TokenKind::Question),
    punctuation("(", TokenKind::LParen),
    punctuation(")", TokenKind::RParen),
    punctuation("[", TokenKind::LBracket),
    punctuation("]", TokenKind::RBracket),
    punctuation("{", TokenKind::LBrace),
    punctuation("}", TokenKind::RBrace),
];

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Whether `text` is a spelling the lexer would tokenize as a single
/// `TokenKind::Ident` -- i.e. a legal identifier start/continue run that is
/// not one of the reserved keyword spellings. Callers that mint `Ident`s
/// from outside the lexer (filesystem module discovery, CLI-declared module
/// identities) use this so they never admit a spelling the parser itself
/// could not name.
pub fn is_valid_identifier(text: &str) -> bool {
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !is_ident_start(first) || !chars.all(is_ident_continue) {
        return false;
    }
    !FIXED_TOKENS
        .iter()
        .any(|token| token.class == FixedTokenClass::Keyword && token.spelling == text)
}

#[cfg(test)]
mod identifier_tests {
    use super::is_valid_identifier;

    #[test]
    fn accepts_ordinary_identifier_spellings() {
        for name in ["foo", "_foo", "foo123", "foo_bar", "_", "A", "z9"] {
            assert!(is_valid_identifier(name), "expected {name:?} to be valid");
        }
    }

    #[test]
    fn rejects_malformed_spellings() {
        for name in [
            "", "0123", "foo-bar", "foo.bar", "$foo", "foo bar", "foo/bar", "föö",
        ] {
            assert!(
                !is_valid_identifier(name),
                "expected {name:?} to be invalid"
            );
        }
    }

    #[test]
    fn rejects_reserved_keyword_spellings() {
        for name in ["if", "else", "struct", "for", "return", "macro"] {
            assert!(
                !is_valid_identifier(name),
                "expected keyword {name:?} to be invalid"
            );
        }
    }

    #[test]
    fn contextual_words_stay_valid_identifiers() {
        // `mut` and other contextual keywords are still `Ident` tokens; only
        // FIXED_TOKENS' `Keyword`-class spellings are reserved.
        assert!(is_valid_identifier("mut"));
    }
}

pub fn tokenize(source: &str) -> (Vec<Token>, Vec<ParseError>) {
    let lexed = lex(source);
    (lexed.tokens, lexed.errors)
}

pub struct Lexed {
    pub tokens: Vec<Token>,
    pub comments: Vec<Span>,
    pub errors: Vec<ParseError>,
}

pub fn lex(source: &str) -> Lexed {
    let mut lexer = Lexer {
        source,
        pos: 0,
        tokens: Vec::new(),
        comments: Vec::new(),
        errors: Vec::new(),
    };
    lexer.run();
    Lexed {
        tokens: lexer.tokens,
        comments: lexer.comments,
        errors: lexer.errors,
    }
}

struct Lexer<'a> {
    source: &'a str,
    pos: usize,
    tokens: Vec<Token>,
    comments: Vec<Span>,
    errors: Vec<ParseError>,
}

impl<'a> Lexer<'a> {
    fn peek(&self) -> Option<char> {
        self.source[self.pos..].chars().next()
    }

    fn peek_at(&self, n: usize) -> Option<char> {
        self.source[self.pos..].chars().nth(n)
    }

    fn advance(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    fn starts_with(&self, s: &str) -> bool {
        self.source[self.pos..].starts_with(s)
    }

    fn span_from(&self, start: usize) -> Span {
        Span::new(start, self.pos)
    }

    fn run(&mut self) {
        loop {
            self.skip_trivia();
            let start = self.pos;
            let Some(c) = self.peek() else { break };
            match self.scan_token(c, start) {
                Ok(TokenKind::LBrace) if self.at_asm_body_open() => {
                    self.tokens.push(Token {
                        kind: TokenKind::LBrace,
                        span: self.span_from(start),
                        origin: Origin::default(),
                    });
                    self.scan_asm_body();
                }
                Ok(kind) => self.tokens.push(Token {
                    kind,
                    span: self.span_from(start),
                    origin: Origin::default(),
                }),
                Err(err) => self.errors.push(err),
            }
        }
        self.tokens.push(Token {
            kind: TokenKind::Eof,
            span: Span::new(self.pos, self.pos),
            origin: Origin::default(),
        });
    }

    /// Detects the committed `asm(...) => {` shape by walking already-emitted
    /// tokens backward from a fat arrow through the balanced descriptor-list
    /// parens to their opening `asm` identifier. No other Omega grammar
    /// places a code block directly after `=>`, so this is unambiguous.
    fn at_asm_body_open(&self) -> bool {
        let n = self.tokens.len();
        if n < 2 {
            return false;
        }
        if !matches!(self.tokens[n - 1].kind, TokenKind::FatArrow) {
            return false;
        }
        if !matches!(self.tokens[n - 2].kind, TokenKind::RParen) {
            return false;
        }
        let mut depth = 0i32;
        let mut i = n - 2;
        loop {
            match &self.tokens[i].kind {
                TokenKind::RParen => depth += 1,
                TokenKind::LParen => {
                    depth -= 1;
                    if depth == 0 {
                        return i > 0
                            && matches!(&self.tokens[i - 1].kind, TokenKind::Ident(name) if name == "asm");
                    }
                }
                _ => {}
            }
            if i == 0 {
                return false;
            }
            i -= 1;
        }
    }

    /// Captures backend assembly text verbatim, tracking only literal `{`/`}`
    /// depth. Omega comments/strings/tokenization do not apply inside; the
    /// text is forwarded to the backend unchanged.
    fn scan_asm_body(&mut self) {
        let start = self.pos;
        let mut depth = 1i32;
        loop {
            match self.peek() {
                None => {
                    self.errors.push(ParseError::new(
                        self.span_from(start),
                        ParseErrorKind::UnterminatedAsmBody,
                    ));
                    break;
                }
                Some('{') => {
                    depth += 1;
                    self.advance();
                }
                Some('}') => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                    self.advance();
                }
                Some(_) => {
                    self.advance();
                }
            }
        }
        let end = self.pos;
        self.tokens.push(Token {
            kind: TokenKind::AsmBody(self.source[start..end].to_string()),
            span: Span::new(start, end),
            origin: Origin::default(),
        });
        if self.peek() == Some('}') {
            let rbrace_start = self.pos;
            self.advance();
            self.tokens.push(Token {
                kind: TokenKind::RBrace,
                span: self.span_from(rbrace_start),
                origin: Origin::default(),
            });
        }
    }

    fn skip_trivia(&mut self) {
        loop {
            match self.peek() {
                Some(c) if c.is_whitespace() => {
                    self.advance();
                }
                Some('#') => self.skip_comment(),
                _ => break,
            }
        }
    }

    fn skip_comment(&mut self) {
        let start = self.pos;
        let mut hashes = 0usize;
        while self.peek() == Some('#') {
            self.advance();
            hashes += 1;
        }
        if hashes == 1 {
            while let Some(c) = self.peek() {
                if c == '\n' {
                    break;
                }
                self.advance();
            }
            self.comments.push(self.span_from(start));
            return;
        }
        loop {
            match self.peek() {
                None => {
                    self.errors.push(ParseError::new(
                        self.span_from(start),
                        ParseErrorKind::UnterminatedComment,
                    ));
                    self.comments.push(self.span_from(start));
                    return;
                }
                Some('#') => {
                    let mut run = 0usize;
                    while self.peek() == Some('#') {
                        self.advance();
                        run += 1;
                    }
                    if run == hashes {
                        self.comments.push(self.span_from(start));
                        return;
                    }
                }
                Some(_) => {
                    self.advance();
                }
            }
        }
    }

    fn scan_token(&mut self, c: char, start: usize) -> Result<TokenKind, ParseError> {
        match c {
            '$' if self.peek_at(1).is_some_and(is_ident_start) => Ok(self.scan_metavar()),
            '"' => self.scan_string_or_multiline(start),
            'b' if self.peek_at(1) == Some('"') => {
                self.advance(); // 'b'
                let TokenKind::Str(value) = self.scan_string_or_multiline(start)? else {
                    unreachable!("scan_string_or_multiline always produces a Str token")
                };
                Ok(TokenKind::ByteStr(value))
            }
            '\'' => self.scan_char(start),
            c if c.is_ascii_digit() => Ok(self.scan_number()),
            c if is_ident_start(c) => Ok(self.scan_ident()),
            _ => self.scan_punct(start),
        }
    }

    fn scan_ident(&mut self) -> TokenKind {
        let start = self.pos;
        self.advance();
        while self.peek().is_some_and(is_ident_continue) {
            self.advance();
        }
        let text = &self.source[start..self.pos];
        FIXED_TOKENS
            .iter()
            .find(|token| token.class == FixedTokenClass::Keyword && token.spelling == text)
            .map(|token| token.kind.clone())
            .unwrap_or_else(|| TokenKind::Ident(text.to_string()))
    }

    fn scan_metavar(&mut self) -> TokenKind {
        self.advance(); // '$'
        let name_start = self.pos;
        self.advance();
        while self.peek().is_some_and(is_ident_continue) {
            self.advance();
        }
        TokenKind::Metavar(self.source[name_start..self.pos].to_string())
    }

    fn scan_punct(&mut self, start: usize) -> Result<TokenKind, ParseError> {
        if let Some(token) = FIXED_TOKENS
            .iter()
            .filter(|token| token.class == FixedTokenClass::Punctuation)
            .filter(|token| self.starts_with(token.spelling))
            .max_by_key(|token| token.spelling.len())
        {
            self.pos += token.spelling.len();
            return Ok(token.kind.clone());
        }

        let invalid = self
            .peek()
            .expect("caller already confirmed a char is here");
        self.advance();
        Err(ParseError::new(
            self.span_from(start),
            ParseErrorKind::InvalidCharacter(invalid),
        ))
    }

    fn scan_number(&mut self) -> TokenKind {
        let (base, integer_part) = if self.peek() == Some('0') {
            match self.peek_at(1) {
                Some('x') => {
                    self.advance();
                    self.advance();
                    (NumberBase::Hex, self.scan_radix_digits(16))
                }
                Some('o') => {
                    self.advance();
                    self.advance();
                    (NumberBase::Octal, self.scan_radix_digits(8))
                }
                Some('b') => {
                    self.advance();
                    self.advance();
                    (NumberBase::Binary, self.scan_radix_digits(2))
                }
                _ => (NumberBase::Decimal, self.scan_radix_digits(10)),
            }
        } else {
            (NumberBase::Decimal, self.scan_radix_digits(10))
        };

        let fractional_part = if matches!(base, NumberBase::Decimal)
            && self.peek() == Some('.')
            && self.peek_at(1).is_some_and(|c| c.is_ascii_digit())
        {
            self.advance();
            Some(self.scan_radix_digits(10))
        } else {
            None
        };

        let explicit_type = self.scan_number_suffix();

        TokenKind::Number(NumberExpr {
            base,
            integer_part,
            fractional_part,
            explicit_type,
        })
    }

    fn scan_radix_digits(&mut self, radix: u32) -> String {
        let mut out = String::new();
        loop {
            match self.peek() {
                Some(c) if c.is_digit(radix) => {
                    out.push(c);
                    self.advance();
                }
                Some('_') => {
                    self.advance();
                }
                _ => break,
            }
        }
        out
    }

    fn scan_number_suffix(&mut self) -> Option<Ident> {
        if self.try_consume_word("usize") {
            return Some(Ident("usize".to_string()));
        }
        if self.try_consume_word("isize") {
            return Some(Ident("isize".to_string()));
        }
        if matches!(self.peek(), Some('i' | 'u' | 'f'))
            && self.peek_at(1).is_some_and(|c| c.is_ascii_digit())
        {
            let prefix = self.advance().unwrap();
            let mut digits = String::new();
            while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                digits.push(self.advance().unwrap());
            }
            return Some(Ident(format!("{prefix}{digits}")));
        }
        None
    }

    fn try_consume_word(&mut self, word: &str) -> bool {
        if self.starts_with(word) {
            let after = self.pos + word.len();
            let boundary_ok = self.source[after..]
                .chars()
                .next()
                .is_none_or(|c| !is_ident_continue(c));
            if boundary_ok {
                self.pos = after;
                return true;
            }
        }
        false
    }

    fn scan_string_or_multiline(&mut self, start: usize) -> Result<TokenKind, ParseError> {
        let quote_run = self.source[self.pos..]
            .chars()
            .take_while(|&c| c == '"')
            .count();
        if quote_run < 3 {
            return self.scan_string(start);
        }
        for _ in 0..quote_run {
            self.advance();
        }
        if quote_run % 2 == 0 {
            self.errors.push(ParseError::new(
                self.span_from(start),
                ParseErrorKind::EvenMultilineStringDelimiter { count: quote_run },
            ));
        }
        // Raw/verbatim content -- no backslash-escape processing, unlike
        // `scan_string` below.
        let mut content = String::new();
        loop {
            match self.peek() {
                None => {
                    return Err(ParseError::new(
                        self.span_from(start),
                        ParseErrorKind::UnterminatedString,
                    ));
                }
                Some('"') => {
                    let run_start = self.pos;
                    let mut run = 0usize;
                    while self.peek() == Some('"') {
                        self.advance();
                        run += 1;
                    }
                    if run == quote_run {
                        return Ok(TokenKind::Str(content));
                    }
                    // Not a matching run -- ordinary literal content.
                    content.push_str(&self.source[run_start..self.pos]);
                }
                Some(c) => {
                    content.push(c);
                    self.advance();
                }
            }
        }
    }

    fn scan_string(&mut self, start: usize) -> Result<TokenKind, ParseError> {
        self.advance(); // opening quote
        let mut content = String::new();
        loop {
            match self.peek() {
                None => {
                    return Err(ParseError::new(
                        self.span_from(start),
                        ParseErrorKind::UnterminatedString,
                    ));
                }
                Some('"') => {
                    self.advance();
                    return Ok(TokenKind::Str(content));
                }
                Some('\\') => match self.try_scan_escape(start)? {
                    Some(c) => content.push(c),
                    None => {
                        content.push('\\');
                        self.advance();
                    }
                },
                Some(c) => {
                    content.push(c);
                    self.advance();
                }
            }
        }
    }

    fn scan_char(&mut self, start: usize) -> Result<TokenKind, ParseError> {
        self.advance(); // opening quote
        let c = match self.peek() {
            None | Some('\'') => {
                self.recover_char_literal();
                return Err(ParseError::new(
                    self.span_from(start),
                    ParseErrorKind::InvalidCharLiteral,
                ));
            }
            Some('\\') => match self.try_scan_escape(start)? {
                Some(c) => c,
                None => {
                    self.advance();
                    '\\'
                }
            },
            Some(c) => {
                self.advance();
                c
            }
        };
        match self.peek() {
            Some('\'') => {
                self.advance();
                Ok(TokenKind::Char(c))
            }
            None => Err(ParseError::new(
                self.span_from(start),
                ParseErrorKind::UnterminatedChar,
            )),
            Some(_) => {
                self.recover_char_literal();
                Err(ParseError::new(
                    self.span_from(start),
                    ParseErrorKind::InvalidCharLiteral,
                ))
            }
        }
    }

    fn recover_char_literal(&mut self) {
        while let Some(c) = self.peek() {
            if c == '\'' || c == '\n' {
                break;
            }
            self.advance();
        }
        if self.peek() == Some('\'') {
            self.advance();
        }
    }

    fn try_scan_escape(&mut self, literal_start: usize) -> Result<Option<char>, ParseError> {
        let simple = match self.peek_at(1) {
            Some('n') => Some('\n'),
            Some('t') => Some('\t'),
            Some('r') => Some('\r'),
            Some('0') => Some('\0'),
            Some('\\') => Some('\\'),
            Some('\'') => Some('\''),
            Some('"') => Some('"'),
            _ => None,
        };
        if let Some(decoded) = simple {
            self.advance(); // backslash
            self.advance(); // the letter
            return Ok(Some(decoded));
        }
        if self.peek_at(1) == Some('u') {
            return self.try_scan_unicode_escape(literal_start);
        }
        Ok(None)
    }

    fn try_scan_unicode_escape(
        &mut self,
        literal_start: usize,
    ) -> Result<Option<char>, ParseError> {
        if self.peek_at(2) != Some('{') {
            return Ok(None); // structural mismatch -- fall back to a literal '\'
        }
        let mut hex = String::new();
        let mut offset = 3;
        while hex.len() < 6 {
            match self.peek_at(offset) {
                Some(c) if c.is_ascii_hexdigit() => {
                    hex.push(c);
                    offset += 1;
                }
                _ => break,
            }
        }
        if hex.is_empty() || self.peek_at(offset) != Some('}') {
            return Ok(None); // structural mismatch -- fall back to a literal '\'
        }
        for _ in 0..=offset {
            self.advance(); // '\', 'u', '{', the hex digits, '}'
        }
        u32::from_str_radix(&hex, 16)
            .ok()
            .and_then(char::from_u32)
            .map(Some)
            .ok_or_else(|| {
                ParseError::new(
                    self.span_from(literal_start),
                    ParseErrorKind::InvalidUnicodeEscape(hex),
                )
            })
    }
}

#[cfg(test)]
mod multiline_string_tests;
#[cfg(test)]
mod spelling_tests;
