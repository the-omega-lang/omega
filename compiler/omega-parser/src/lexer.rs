use crate::ast::expression::{NumberBase, NumberExpr};
use crate::ast::identifier::Ident;
use crate::ast::identifier::Origin;
use crate::diagnostics::{ParseError, ParseErrorKind, Span};

/// One lexical unit -- everything the parser sees is one of these; comments
/// and whitespace are consumed internally by [`tokenize`] and never turn
/// into tokens at all.
#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    Ident(String),
    /// Captures shape (radix, digit text, suffix), not value -- semantic
    /// analysis still range-checks.
    Number(NumberExpr),
    /// Decoded content (escapes already resolved).
    Str(String),
    /// `b"..."` -- same decoded-content shape as `Str`, tagged separately so
    /// the parser produces a `ByteStringExpr` (a raw byte run, no implicit
    /// null terminator) instead of a `StringExpr`.
    ByteStr(String),
    Char(char),
    /// `$name` -- a macro metavariable, recognized atomically.
    Metavar(String),

    // Deliberately not keywords here: `self`/`mut`/`type`/`usize`/`isize`
    // are context-sensitive (e.g. `self` only in a function's first
    // parameter, `mut` only after `*` or leading a binding) and stay plain
    // `Ident` tokens so they remain usable as ordinary names; the parser
    // recognizes them contextually by comparing ident text.
    True,
    False,
    If,
    Else,
    Match,
    Extern,
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
    /// `...` -- variadic function-type parameters ONLY (`(s: *u8, ...) =>
    /// i32`); not a range operator. See `ast::range::RangeExpr`'s doc comment.
    DotDotDot,
    ColonColon,
    FatArrow,
    ColonEq,
    EqEq,
    NotEq,
    LtEq,
    GtEq,
    /// `..=` -- an inclusive-end range; always requires an explicit end
    /// (`a..=` alone is a parse error).
    DotDotEq,
    /// `..<` -- an exclusive-end range; always requires an explicit end.
    DotDotLt,
    /// `..` -- an open range with no end, ever; the only range spelling
    /// legal with nothing written. Its bound is inferred from context; see
    /// `ast::range::RangeExpr`'s doc comment.
    DotDot,
    PlusPlus,
    MinusMinus,
    /// `&&` -- short-circuiting logical AND. Unlike `&`, the right operand
    /// is evaluated only when the left is `true`; see `LogicalOp`.
    AmpAmp,
    /// `||` -- short-circuiting logical OR; see `LogicalOp`.
    PipePipe,
    /// `<<` -- see `BinaryOp::Shl`.
    Shl,
    /// `>>` -- see `BinaryOp::Shr`.
    Shr,
    /// `+= -= *= /= %= &= |= ^= <<= >>=` -- an "operate and assign" of the
    /// matching `BinaryOp`, desugared during analysis (see `Analyzer::
    /// analyze_compound_assign`) into `target = target op value`.
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

    // Single-char punctuation.
    /// `$` as the macro invocation suffix (`name$(...)`) and repetition
    /// prefix (`$...`); `$name` remains a `Metavar` token.
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
    /// `|` -- see `BinaryOp::BitOr`.
    Pipe,
    /// `^` -- see `BinaryOp::BitXor`.
    Caret,
    /// `~base` -- unary bitwise-not on an integer; see `BitNotExpr`.
    Tilde,
    /// `!base` -- unary logical-not on a `bool`; see `NotExpr`. Distinct
    /// from `Tilde`: `~` is a bit pattern operation and is rejected on
    /// `bool` (flipping `0`/`1`'s bits leaves `{0,1}`), while `!` is
    /// defined only on `bool`.
    Not,
    /// `@` -- leads an item annotation (`@inline(always)`); see
    /// `parser::item::parse_annotations`.
    At,
    /// `?` -- marks `[?]T`, an unsized array; see
    /// `parser::r#type::parse_bracket_type`.
    Question,

    // Delimiters -- flat, individual tokens; nesting is the parser's
    // concern, not the lexer's (unlike the old macro-only `Token::Group`).
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,

    Eof,
}

impl TokenKind {
    /// The exact source spelling of a fixed token. Payload-bearing tokens and
    /// end-of-input have no fixed spelling.
    pub fn spelling(&self) -> Option<&'static str> {
        match self {
            Self::Ident(_)
            | Self::Number(_)
            | Self::Str(_)
            | Self::ByteStr(_)
            | Self::Char(_)
            | Self::Metavar(_)
            | Self::Eof => None,
            Self::True => Some("true"),
            Self::False => Some("false"),
            Self::If => Some("if"),
            Self::Else => Some("else"),
            Self::Match => Some("match"),
            Self::Extern => Some("extern"),
            Self::Import => Some("import"),
            Self::Return => Some("return"),
            Self::Struct => Some("struct"),
            Self::Enum => Some("enum"),
            Self::Union => Some("union"),
            Self::Spec => Some("spec"),
            Self::While => Some("while"),
            Self::Loop => Some("loop"),
            Self::For => Some("for"),
            Self::Break => Some("break"),
            Self::Continue => Some("continue"),
            Self::Defer => Some("defer"),
            Self::Macro => Some("macro"),
            Self::DotDotDot => Some("..."),
            Self::ColonColon => Some("::"),
            Self::FatArrow => Some("=>"),
            Self::ColonEq => Some(":="),
            Self::EqEq => Some("=="),
            Self::NotEq => Some("!="),
            Self::AmpAmp => Some("&&"),
            Self::PipePipe => Some("||"),
            Self::LtEq => Some("<="),
            Self::GtEq => Some(">="),
            Self::DotDotEq => Some("..="),
            Self::DotDotLt => Some("..<"),
            Self::DotDot => Some(".."),
            Self::PlusPlus => Some("++"),
            Self::MinusMinus => Some("--"),
            Self::Shl => Some("<<"),
            Self::Shr => Some(">>"),
            Self::PlusEq => Some("+="),
            Self::MinusEq => Some("-="),
            Self::StarEq => Some("*="),
            Self::SlashEq => Some("/="),
            Self::PercentEq => Some("%="),
            Self::AmpEq => Some("&="),
            Self::PipeEq => Some("|="),
            Self::CaretEq => Some("^="),
            Self::ShlEq => Some("<<="),
            Self::ShrEq => Some(">>="),
            Self::Dollar => Some("$"),
            Self::Percent => Some("%"),
            Self::Amp => Some("&"),
            Self::Star => Some("*"),
            Self::Plus => Some("+"),
            Self::Comma => Some(","),
            Self::Minus => Some("-"),
            Self::Dot => Some("."),
            Self::Slash => Some("/"),
            Self::Colon => Some(":"),
            Self::Semi => Some(";"),
            Self::Lt => Some("<"),
            Self::Eq => Some("="),
            Self::Gt => Some(">"),
            Self::Pipe => Some("|"),
            Self::Caret => Some("^"),
            Self::Tilde => Some("~"),
            Self::Not => Some("!"),
            Self::At => Some("@"),
            Self::Question => Some("?"),
            Self::LParen => Some("("),
            Self::RParen => Some(")"),
            Self::LBracket => Some("["),
            Self::RBracket => Some("]"),
            Self::LBrace => Some("{"),
            Self::RBrace => Some("}"),
        }
    }

    /// A short, human-readable name for "found X" diagnostics.
    pub fn describe(&self) -> String {
        match self {
            Self::Ident(s) => format!("identifier '{s}'"),
            Self::Number(_) => "a number literal".to_string(),
            Self::Str(_) => "a string literal".to_string(),
            Self::ByteStr(_) => "a binary string literal".to_string(),
            Self::Char(_) => "a character literal".to_string(),
            Self::Metavar(s) => format!("'${s}'"),
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


/// Multi-character punctuation. `scan_punct` selects the longest matching
/// spelling, so table order cannot affect maximal-munch behavior.
const MULTI_CHAR_PUNCT: &[(&str, TokenKind)] = &[
    ("...", TokenKind::DotDotDot),
    ("..=", TokenKind::DotDotEq),
    ("..<", TokenKind::DotDotLt),
    ("..", TokenKind::DotDot),
    ("::", TokenKind::ColonColon),
    ("=>", TokenKind::FatArrow),
    (":=", TokenKind::ColonEq),
    ("==", TokenKind::EqEq),
    ("!=", TokenKind::NotEq),
    ("<=", TokenKind::LtEq),
    (">=", TokenKind::GtEq),
    ("++", TokenKind::PlusPlus),
    ("--", TokenKind::MinusMinus),
    ("<<=", TokenKind::ShlEq),
    (">>=", TokenKind::ShrEq),
    ("<<", TokenKind::Shl),
    (">>", TokenKind::Shr),
    ("+=", TokenKind::PlusEq),
    ("-=", TokenKind::MinusEq),
    ("*=", TokenKind::StarEq),
    ("/=", TokenKind::SlashEq),
    ("%=", TokenKind::PercentEq),
    ("&&", TokenKind::AmpAmp),
    ("||", TokenKind::PipePipe),
    ("&=", TokenKind::AmpEq),
    ("|=", TokenKind::PipeEq),
    ("^=", TokenKind::CaretEq),
];

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Tokenizes a whole source file, consuming comments/whitespace internally.
/// Recovers from lexical errors rather than aborting: an unexpected
/// character is skipped and lexing continues; an unterminated
/// string/char/comment consumes to end-of-input.
pub fn tokenize(source: &str) -> (Vec<Token>, Vec<ParseError>) {
    let lexed = lex(source);
    (lexed.tokens, lexed.errors)
}

/// [`tokenize`]'s full output, comment spans included. The parser never
/// wants comments (that's the whole point of consuming them as trivia), but
/// the diagnostics highlighter does -- a snippet's comments should render
/// dimmed, same as every other token class gets its color.
pub struct Lexed {
    pub tokens: Vec<Token>,
    /// Each comment's whole span (single-line and multi-line alike), in
    /// source order.
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
    /// A *byte* offset into `source`, not a char index -- `Span`s are byte
    /// ranges.
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

    /// Mirrors the old `trivia::comment`'s hashes-counting rule exactly:
    /// `#` alone is a single-line comment (to EOL/EOF); a run of N >= 2
    /// `#`s starts a multi-line comment closed only by a run of exactly N
    /// `#`s. An unterminated multi-line comment records an error and
    /// consumes to EOF, rather than aborting the whole tokenize pass.
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
            '$' => {
                self.advance();
                Ok(TokenKind::Dollar)
            }
            '"' => self.scan_string_or_multiline(start),
            // `b"..."` -- only committed when `"` immediately follows, so
            // `b` alone or an identifier starting with `b` is untouched.
            'b' if self.peek_at(1) == Some('"') => {
                self.advance(); // 'b'
                let TokenKind::Str(s) = self.scan_string_or_multiline(start)? else {
                    unreachable!("scan_string_or_multiline always produces a Str token")
                };
                Ok(TokenKind::ByteStr(s))
            }
            '\'' => self.scan_char(start),
            '(' => {
                self.advance();
                Ok(TokenKind::LParen)
            }
            ')' => {
                self.advance();
                Ok(TokenKind::RParen)
            }
            '[' => {
                self.advance();
                Ok(TokenKind::LBracket)
            }
            ']' => {
                self.advance();
                Ok(TokenKind::RBracket)
            }
            '{' => {
                self.advance();
                Ok(TokenKind::LBrace)
            }
            '}' => {
                self.advance();
                Ok(TokenKind::RBrace)
            }
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
        match text {
            "true" => TokenKind::True,
            "false" => TokenKind::False,
            "if" => TokenKind::If,
            "else" => TokenKind::Else,
            "match" => TokenKind::Match,
            "extern" => TokenKind::Extern,
            "import" => TokenKind::Import,
            "return" => TokenKind::Return,
            "struct" => TokenKind::Struct,
            "enum" => TokenKind::Enum,
            "union" => TokenKind::Union,
            "spec" => TokenKind::Spec,
            "while" => TokenKind::While,
            "loop" => TokenKind::Loop,
            "for" => TokenKind::For,
            "break" => TokenKind::Break,
            "continue" => TokenKind::Continue,
            "defer" => TokenKind::Defer,
            "macro" => TokenKind::Macro,
            _ => TokenKind::Ident(text.to_string()),
        }
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
        let matched = MULTI_CHAR_PUNCT
            .iter()
            .filter(|(op, _)| self.starts_with(op))
            .max_by_key(|(op, _)| op.len());
        if let Some((op, kind)) = matched {
            self.pos += op.len();
            return Ok(kind.clone());
        }
        let c = self
            .peek()
            .expect("caller already confirmed a char is here");
        let kind = match c {
            '%' => TokenKind::Percent,
            '&' => TokenKind::Amp,
            '*' => TokenKind::Star,
            '+' => TokenKind::Plus,
            ',' => TokenKind::Comma,
            '-' => TokenKind::Minus,
            '.' => TokenKind::Dot,
            '/' => TokenKind::Slash,
            ':' => TokenKind::Colon,
            ';' => TokenKind::Semi,
            '<' => TokenKind::Lt,
            '=' => TokenKind::Eq,
            '>' => TokenKind::Gt,
            '|' => TokenKind::Pipe,
            '^' => TokenKind::Caret,
            '~' => TokenKind::Tilde,
            '!' => TokenKind::Not,
            '@' => TokenKind::At,
            '?' => TokenKind::Question,
            _ => {
                self.advance();
                return Err(ParseError::new(
                    self.span_from(start),
                    ParseErrorKind::InvalidCharacter(c),
                ));
            }
        };
        self.advance();
        Ok(kind)
    }

    // --- Literals ---

    /// Mirrors `NumberExpr::parser`'s exact shape (see that type's doc
    /// comment) -- adjacency matters: no whitespace is tolerated between a
    /// based prefix and its digits, between the digit run and a decimal
    /// point, or between the digits and a type suffix. Doesn't validate the
    /// value (radix-correctness, suffix range, ...), only consumes the
    /// right character shape as one atom -- semantic analysis still does
    /// real validation.
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

    /// One or more base-`radix` digits, `_` allowed anywhere after the
    /// first as a visual separator (stripped from the result) -- matching
    /// `radix_digits`'s existing rule exactly. Assumes the caller already
    /// confirmed a valid first digit is present.
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

    /// `usize`/`isize` (tried first, whole-word so `5isize` isn't parsed as
    /// `5i` + a dangling `size`), or `i`/`u`/`f` + decimal digits.
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

    /// Consumes `word` if it's here *and* isn't immediately followed by
    /// another identifier character (so e.g. `usizeish` doesn't wrongly
    /// match a `usize` suffix) -- mirrors `text::keyword`'s word-boundary
    /// check.
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

    /// Dispatches between an ordinary single-quote string (a run of exactly
    /// 2 quotes) and a multi-line string (`"""..."""`, N >= 3 quotes, closed
    /// by a matching run of N, same hashes-counting shape as
    /// `skip_comment`). An even N is rejected with a dedicated diagnostic
    /// rather than silently reinterpreted, since it could be confused with
    /// two shorter, separately-closed runs.
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

    /// Exactly one character or one escape between the quotes -- an empty
    /// (`''`) or multi-character literal is `InvalidCharLiteral`. On a
    /// malformed shape, skips to the literal's closing `'` (or
    /// newline/EOF) before returning, so e.g. `'ab'` doesn't cascade into a
    /// second spurious error from the leftover `b'`.
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

    /// `\n \t \r \0 \\ \" \' \u{XXXX}`. Called with `self.peek() ==
    /// Some('\\')`. An *unrecognized* escape letter (e.g. `\q`) is not an
    /// error: returns `Ok(None)` so the caller treats the backslash as a
    /// literal character and reads the next character normally. A
    /// `\u{...}` that's structurally well-formed but names an invalid
    /// Unicode scalar value is a real `InvalidUnicodeEscape` error -- the
    /// one case that doesn't fall back silently, since the delimiter/digit
    /// structure already committed.
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

/// `spelling()` and the scanners must stay one fact: every fixed token's
/// spelling has to lex back to that same token, as *one* token. This is what
/// makes deriving `describe()` from `spelling()` safe, and it catches a
/// maximal-munch mistake directly -- `<<=` lexing as `<` `<=` fails here.
///
/// The witness list below is written independently of the tables it checks,
/// on purpose: a test that read the same table it verifies would agree with
/// any edit, including a wrong one.
#[cfg(test)]
mod spelling_tests {
    use super::{MULTI_CHAR_PUNCT, TokenKind, lex};

    /// Lexes `source`, asserting it produced exactly one token, and returns
    /// it.
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
            .map(|t| &t.kind)
            .filter(|k| !matches!(k, TokenKind::Eof))
            .collect();
        assert_eq!(
            kinds.len(),
            1,
            "`{source}` must lex as exactly one token, got {kinds:?}"
        );
        kinds[0].clone()
    }

    /// Every fixed token this crate can produce, as source text. Keywords
    /// come from `scan_ident`'s match, punctuation from `scan_punct`'s
    /// single-character match; the multi-character forms are checked against
    /// `MULTI_CHAR_PUNCT` itself in the test below.
    const FIXED: &[&str] = &[
        "true", "false", "if", "else", "match", "extern", "import", "return", "struct", "enum",
        "union", "spec", "while", "loop", "for", "break", "continue", "defer", "macro", "%", "&",
        "*", "+", ",", "-", ".", "/", ":", ";", "<", "=", ">", "|", "^", "~", "!", "@", "?", "(",
        ")", "[", "]", "{", "}",
    ];

    #[test]
    fn every_fixed_spelling_lexes_back_to_its_own_token() {
        for text in FIXED {
            let kind = sole_token(text);
            assert_eq!(
                kind.spelling(),
                Some(*text),
                "`{text}` lexed as {kind:?}, whose spelling disagrees"
            );
        }
    }

    #[test]
    fn every_multi_char_punct_lexes_as_one_token() {
        // Directly the maximal-munch guard: `<<=` must not lex as `<` `<=`,
        // and `..=` must not lex as `..` `=`.
        for (text, expected) in MULTI_CHAR_PUNCT {
            let kind = sole_token(text);
            assert_eq!(&kind, expected, "`{text}` lexed as {kind:?}");
            assert_eq!(kind.spelling(), Some(*text));
        }
    }

    #[test]
    fn describe_agrees_with_spelling_for_fixed_tokens() {
        for text in FIXED {
            assert_eq!(sole_token(text).describe(), format!("'{text}'"));
        }
    }
}

#[cfg(test)]
mod multiline_string_tests {
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
}
