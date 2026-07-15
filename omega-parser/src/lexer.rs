use crate::ast::expression::number::{NumberBase, NumberExpr};
use crate::ast::identifier::Ident;
use crate::diagnostics::{ParseError, ParseErrorKind, Span};

/// One lexical unit -- everything the parser sees is one of these; comments
/// and whitespace are consumed internally by [`tokenize`] and never turn
/// into tokens at all, which is what lets every parsing function stay
/// completely unaware of trivia (unlike the old scannerless grammar, where
/// `.trivia_padded()` had to be threaded through nearly every combinator by
/// hand).
#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    Ident(String),
    /// Captures shape (radix, digit text, suffix), not value -- semantic
    /// analysis still range-checks; see `NumberExpr`'s own doc comment.
    Number(NumberExpr),
    /// Decoded content (escapes already resolved) -- matches `StringExpr`'s
    /// existing shape, which wraps a decoded `String` the same way.
    Str(String),
    /// `b"..."` -- same decoded-content shape and escape rules as `Str`
    /// (see `Lexer::scan_string`, shared verbatim), just tagged separately
    /// so the parser knows to produce a `ByteStringExpr` instead of a
    /// `StringExpr`: a raw byte run with no implicit null terminator, not a
    /// C-style string.
    ByteStr(String),
    Char(char),
    /// `$name` -- only meaningful inside a macro definition's body; `$` has
    /// exactly one use in this grammar, so it's recognized as one atomic
    /// token rather than a separate `$` punctuation token the parser would
    /// have to pair up with a following identifier itself.
    Metavar(String),

    // Keywords. Deliberately *not* included here: `self`/`mut`/`expr`/
    // `type`/`items`/`usize`/`isize` -- these are context-sensitive in the
    // current grammar (e.g. `self` is a keyword only in a function's
    // first-parameter position, and an ordinary identifier everywhere else,
    // including in expression position inside a method body referencing
    // that same parameter; `mut` is a keyword only immediately after `*`
    // (`*mut T`) or leading a binding declaration (`mut a := ...`), and an
    // ordinary identifier everywhere else; `usize`/`isize` are never syntax
    // keywords at all, just ordinary type-name identifiers plus a narrow
    // number-literal-suffix recognition rule inside `scan_number_suffix`).
    // Reserving them globally here would be a real (if minor) grammar
    // narrowing -- e.g. it would stop `self`/`mut`/`type`/... from being
    // usable as ordinary variable names -- so they stay plain `Ident`
    // tokens, and the parser recognizes them contextually by comparing an
    // ident's text exactly where the current grammar already does.
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
    For,
    Break,
    Continue,
    Defer,
    Macro,

    // Multi-char punctuation, maximal-munch (tried longest-first during
    // lexing so e.g. `...` is never mistaken for `..` followed by `.`).
    /// `...` -- an inclusive range (`a...b`, `a...`, `...b`, or bare `...`
    /// for the full domain), any bound optional; also still used, unchanged,
    /// for variadic function parameters -- the two never collide since a
    /// variadic `...` only ever appears as the last item of a parameter
    /// list, a position expressions never occur in. See `ast::range::RangeExpr`.
    DotDotDot,
    ColonColon,
    FatArrow,
    ColonEq,
    EqEq,
    NotEq,
    LtEq,
    GtEq,
    /// `..<` -- an exclusive-end range (`a..<b`, `..<b`); always requires an
    /// explicit end (`a..<` alone is a parse error, see
    /// `parser::expression::parse_range_tail`). Plain two-dot `..` doesn't
    /// exist in this grammar at all -- every range is spelled either `...`
    /// (inclusive) or `..<` (exclusive-end).
    DotDotLt,
    PlusPlus,
    MinusMinus,
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
    Bang,
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
    /// `~base` -- unary bitwise-not; see `BitNotExpr`.
    Tilde,

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
    /// A short, human-readable name for "found X" diagnostics.
    pub fn describe(&self) -> String {
        match self {
            Self::Ident(s) => format!("identifier '{s}'"),
            Self::Number(_) => "a number literal".to_string(),
            Self::Str(_) => "a string literal".to_string(),
            Self::ByteStr(_) => "a binary string literal".to_string(),
            Self::Char(_) => "a character literal".to_string(),
            Self::Metavar(s) => format!("'${s}'"),
            Self::True => "'true'".to_string(),
            Self::False => "'false'".to_string(),
            Self::If => "'if'".to_string(),
            Self::Else => "'else'".to_string(),
            Self::Match => "'match'".to_string(),
            Self::Extern => "'extern'".to_string(),
            Self::Import => "'import'".to_string(),
            Self::Return => "'return'".to_string(),
            Self::Struct => "'struct'".to_string(),
            Self::Enum => "'enum'".to_string(),
            Self::Union => "'union'".to_string(),
            Self::Spec => "'spec'".to_string(),
            Self::While => "'while'".to_string(),
            Self::For => "'for'".to_string(),
            Self::Break => "'break'".to_string(),
            Self::Continue => "'continue'".to_string(),
            Self::Defer => "'defer'".to_string(),
            Self::Macro => "'macro'".to_string(),
            Self::DotDotDot => "'...'".to_string(),
            Self::ColonColon => "'::'".to_string(),
            Self::FatArrow => "'=>'".to_string(),
            Self::ColonEq => "':='".to_string(),
            Self::EqEq => "'=='".to_string(),
            Self::NotEq => "'!='".to_string(),
            Self::LtEq => "'<='".to_string(),
            Self::GtEq => "'>='".to_string(),
            Self::DotDotLt => "'..<'".to_string(),
            Self::PlusPlus => "'++'".to_string(),
            Self::MinusMinus => "'--'".to_string(),
            Self::Shl => "'<<'".to_string(),
            Self::Shr => "'>>'".to_string(),
            Self::PlusEq => "'+='".to_string(),
            Self::MinusEq => "'-='".to_string(),
            Self::StarEq => "'*='".to_string(),
            Self::SlashEq => "'/='".to_string(),
            Self::PercentEq => "'%='".to_string(),
            Self::AmpEq => "'&='".to_string(),
            Self::PipeEq => "'|='".to_string(),
            Self::CaretEq => "'^='".to_string(),
            Self::ShlEq => "'<<='".to_string(),
            Self::ShrEq => "'>>='".to_string(),
            Self::Bang => "'!'".to_string(),
            Self::Percent => "'%'".to_string(),
            Self::Amp => "'&'".to_string(),
            Self::Star => "'*'".to_string(),
            Self::Plus => "'+'".to_string(),
            Self::Comma => "','".to_string(),
            Self::Minus => "'-'".to_string(),
            Self::Dot => "'.'".to_string(),
            Self::Slash => "'/'".to_string(),
            Self::Colon => "':'".to_string(),
            Self::Semi => "';'".to_string(),
            Self::Lt => "'<'".to_string(),
            Self::Eq => "'='".to_string(),
            Self::Gt => "'>'".to_string(),
            Self::Pipe => "'|'".to_string(),
            Self::Caret => "'^'".to_string(),
            Self::Tilde => "'~'".to_string(),
            Self::LParen => "'('".to_string(),
            Self::RParen => "')'".to_string(),
            Self::LBracket => "'['".to_string(),
            Self::RBracket => "']'".to_string(),
            Self::LBrace => "'{'".to_string(),
            Self::RBrace => "'}'".to_string(),
            Self::Eof => "end of input".to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

const KEYWORDS: &[(&str, TokenKind)] = &[
    ("true", TokenKind::True),
    ("false", TokenKind::False),
    ("if", TokenKind::If),
    ("else", TokenKind::Else),
    ("match", TokenKind::Match),
    ("extern", TokenKind::Extern),
    ("import", TokenKind::Import),
    ("return", TokenKind::Return),
    ("struct", TokenKind::Struct),
    ("enum", TokenKind::Enum),
    ("union", TokenKind::Union),
    ("spec", TokenKind::Spec),
    ("while", TokenKind::While),
    ("for", TokenKind::For),
    ("break", TokenKind::Break),
    ("continue", TokenKind::Continue),
    ("defer", TokenKind::Defer),
    ("macro", TokenKind::Macro),
];

/// Tried longest-first so e.g. `...` is never mistaken for `..` + `.`, and
/// `:=` is never mistaken for `:` + `=`.
const MULTI_CHAR_PUNCT: &[(&str, TokenKind)] = &[
    ("...", TokenKind::DotDotDot),
    ("..<", TokenKind::DotDotLt),
    ("::", TokenKind::ColonColon),
    ("=>", TokenKind::FatArrow),
    (":=", TokenKind::ColonEq),
    ("==", TokenKind::EqEq),
    ("!=", TokenKind::NotEq),
    ("<=", TokenKind::LtEq),
    (">=", TokenKind::GtEq),
    ("++", TokenKind::PlusPlus),
    ("--", TokenKind::MinusMinus),
    // The 3-char shift-assign forms must precede their 2-char `<<`/`>>`
    // prefixes below -- maximal-munch here is first-match-wins by list
    // order, not by length.
    ("<<=", TokenKind::ShlEq),
    (">>=", TokenKind::ShrEq),
    ("<<", TokenKind::Shl),
    (">>", TokenKind::Shr),
    ("+=", TokenKind::PlusEq),
    ("-=", TokenKind::MinusEq),
    ("*=", TokenKind::StarEq),
    ("/=", TokenKind::SlashEq),
    ("%=", TokenKind::PercentEq),
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
/// character is skipped (one character) and lexing continues; an
/// unterminated string/char/comment consumes to end-of-input (the same
/// severity the old grammar gave these, just no longer aborting the entire
/// tokenize pass).
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
    let mut lexer = Lexer { source, pos: 0, tokens: Vec::new(), comments: Vec::new(), errors: Vec::new() };
    lexer.run();
    Lexed { tokens: lexer.tokens, comments: lexer.comments, errors: lexer.errors }
}

struct Lexer<'a> {
    source: &'a str,
    /// A *byte* offset into `source` -- not a char index. `Span`s are byte
    /// ranges (matching how `&str` slicing and `LineIndex` both work), so
    /// tracking anything else here would silently produce wrong spans for
    /// any source containing multi-byte UTF-8 characters (e.g. inside a
    /// string literal).
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
                Ok(kind) => self.tokens.push(Token { kind, span: self.span_from(start) }),
                Err(err) => self.errors.push(err),
            }
        }
        self.tokens.push(Token { kind: TokenKind::Eof, span: Span::new(self.pos, self.pos) });
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
                    self.errors.push(ParseError::new(self.span_from(start), ParseErrorKind::UnterminatedComment));
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
            '$' => self.scan_metavar(start),
            '"' => self.scan_string(start),
            // `b"..."` -- checked ahead of the general `is_ident_start`
            // branch below (`'b'` would otherwise just start an ordinary
            // identifier); only committed when a `"` immediately follows,
            // so `b` alone, or an identifier merely starting with `b`
            // (`bar`, `byte_count`, ...), is untouched.
            'b' if self.peek_at(1) == Some('"') => {
                self.advance(); // 'b'
                let TokenKind::Str(s) = self.scan_string(start)? else {
                    unreachable!("scan_string always produces a Str token")
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
        for (word, kind) in KEYWORDS {
            if text == *word {
                return kind.clone();
            }
        }
        TokenKind::Ident(text.to_string())
    }

    fn scan_metavar(&mut self, start: usize) -> Result<TokenKind, ParseError> {
        self.advance(); // '$'
        let name_start = self.pos;
        if !self.peek().is_some_and(is_ident_start) {
            return Err(ParseError::new(self.span_from(start), ParseErrorKind::InvalidMetavariable));
        }
        self.advance();
        while self.peek().is_some_and(is_ident_continue) {
            self.advance();
        }
        Ok(TokenKind::Metavar(self.source[name_start..self.pos].to_string()))
    }

    fn scan_punct(&mut self, start: usize) -> Result<TokenKind, ParseError> {
        for (op, kind) in MULTI_CHAR_PUNCT {
            if self.starts_with(op) {
                self.pos += op.len();
                return Ok(kind.clone());
            }
        }
        let c = self.peek().expect("caller already confirmed a char is here");
        let kind = match c {
            '!' => TokenKind::Bang,
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
            _ => {
                self.advance();
                return Err(ParseError::new(self.span_from(start), ParseErrorKind::InvalidCharacter(c)));
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

        TokenKind::Number(NumberExpr { base, integer_part, fractional_part, explicit_type })
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
        if matches!(self.peek(), Some('i' | 'u' | 'f')) && self.peek_at(1).is_some_and(|c| c.is_ascii_digit()) {
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
            let boundary_ok = self.source[after..].chars().next().is_none_or(|c| !is_ident_continue(c));
            if boundary_ok {
                self.pos = after;
                return true;
            }
        }
        false
    }

    fn scan_string(&mut self, start: usize) -> Result<TokenKind, ParseError> {
        self.advance(); // opening quote
        let mut content = String::new();
        loop {
            match self.peek() {
                None => return Err(ParseError::new(self.span_from(start), ParseErrorKind::UnterminatedString)),
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
    /// (`''`) or multi-character literal is `InvalidCharLiteral`. On either
    /// malformed-shape error, skips forward to the literal's own closing
    /// `'` (or a newline/EOF, whichever comes first) before returning, so
    /// the next token starts cleanly after the whole malformed literal --
    /// without this, e.g. `'ab'`'s trailing `b'` would otherwise get
    /// re-lexed as unrelated fragments (a stray `Ident("b")` token, then a
    /// *second*, spurious error from the leftover `'`), cascading one
    /// mistake into several confusing ones.
    fn scan_char(&mut self, start: usize) -> Result<TokenKind, ParseError> {
        self.advance(); // opening quote
        let c = match self.peek() {
            None | Some('\'') => {
                self.recover_char_literal();
                return Err(ParseError::new(self.span_from(start), ParseErrorKind::InvalidCharLiteral));
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
            None => Err(ParseError::new(self.span_from(start), ParseErrorKind::UnterminatedChar)),
            Some(_) => {
                self.recover_char_literal();
                Err(ParseError::new(self.span_from(start), ParseErrorKind::InvalidCharLiteral))
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

    /// `\n \t \r \0 \\ \" \' \u{XXXX}`, matching `escape::escape_sequence`.
    /// Called with `self.peek() == Some('\\')`.
    ///
    /// Mirrors the old grammar's exact (if slightly unusual) fallback
    /// behavior for anything else: an *unrecognized* escape letter (e.g.
    /// `\q`) is not an error at all -- it silently falls back to treating
    /// the backslash as a literal character, leaving the following
    /// character to be read normally on the next iteration (this is what
    /// `choice((escape_sequence(), any().and_is(quote.not())))` already
    /// does today: `escape_sequence()`'s inner `choice` has no matching
    /// arm, so the *whole* combinator fails and backtracks to before the
    /// backslash, and the outer `any()` picks up just the backslash on its
    /// own). Returns `Ok(None)` for exactly this "no escape recognized
    /// here, caller should treat `\` as a literal and not advance past it
    /// itself" case; the caller (`scan_string`/`scan_char`) does the actual
    /// `content.push('\\'); self.advance();`. A `\u{...}` that *is*
    /// structurally well-formed but names an invalid Unicode scalar value
    /// is a real, hard `InvalidUnicodeEscape` error, matching today's
    /// `try_map` failure there -- the one case that doesn't fall back
    /// silently, since the delimiter/digit structure already committed.
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

    fn try_scan_unicode_escape(&mut self, literal_start: usize) -> Result<Option<char>, ParseError> {
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
            .ok_or_else(|| ParseError::new(self.span_from(literal_start), ParseErrorKind::InvalidUnicodeEscape(hex)))
    }
}
