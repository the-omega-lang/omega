use crate::syntax::ParseError;
use crate::syntax::trivia::TriviaExt;
use chumsky::{error::Rich, input::InputRef, prelude::*};

/// Which kind of bracket a [`Token::Group`] was delimited by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delimiter {
    Paren,
    Bracket,
    Brace,
}

impl Delimiter {
    fn open(self) -> char {
        match self {
            Self::Paren => '(',
            Self::Bracket => '[',
            Self::Brace => '{',
        }
    }

    fn close(self) -> char {
        match self {
            Self::Paren => ')',
            Self::Bracket => ']',
            Self::Brace => '}',
        }
    }
}

/// One lexical unit of a macro body or invocation argument -- the only place
/// in this otherwise-scannerless grammar (every other parser in `syntax/`
/// works directly against `&str`, see `syntax/mod.rs`'s `parser!` macro) that
/// needs real token-stream semantics, because macros are fundamentally a
/// token-level feature: a macro's body is captured raw at definition time
/// (it isn't valid `Expression`/`Statement`/`RootStatement` syntax on its
/// own -- it contains `$name` metavariables, and for an `items`-output
/// macro, syntax like `struct $name { ... }` that only becomes valid after
/// substitution), and only re-parsed through the ordinary grammar once a
/// concrete invocation's arguments have been spliced in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    Ident(String),
    /// `$name` -- only meaningful inside a macro definition's body.
    Metavar(String),
    /// Maximal-munch operator/symbol text, e.g. `"+"`, `"=>"`, `"::"`, `","`.
    Punct(String),
    /// A number/string/char literal, captured verbatim (radix prefix,
    /// underscore separators, type suffix, quotes, and escapes all intact)
    /// so it round-trips exactly through `render` and back through the real
    /// literal grammars (`NumberExpr`/`StringExpr`/`CharExpr`) unchanged.
    Literal(String),
    Group(Delimiter, Vec<Token>),
}

/// Renders a token sequence back to source text for re-parsing through the
/// ordinary (non-token-based) grammar. Always joins with a single space:
/// this is safe for this whitespace-insensitive, `.trivia_padded()`-
/// pervasive grammar precisely because every multi-char operator is already
/// captured as one atomic `Punct` token (e.g. `"=>"`, never two adjacent
/// single-char ones) and every literal was captured as one atomic, correctly
/// -shaped `Literal` token (see `Scanner::scan_number`/`scan_string` below) --
/// so joining never accidentally fuses two tokens into a different token,
/// and never needs to guess adjacency intent across a substitution boundary.
pub fn render(tokens: &[Token]) -> String {
    tokens.iter().map(render_one).collect::<Vec<_>>().join(" ")
}

fn render_one(token: &Token) -> String {
    match token {
        Token::Ident(s) => s.clone(),
        Token::Metavar(s) => format!("${s}"),
        Token::Punct(s) => s.clone(),
        Token::Literal(s) => s.clone(),
        Token::Group(delim, inner) => format!("{}{}{}", delim.open(), render(inner), delim.close()),
    }
}

/// Parses a `{ ... }`/`[ ... ]`/`( ... )` group (matching `delim`) into a
/// flat token list of its contents, recursing into any nested groups. This
/// is the entry point a macro definition's body (`Delimiter::Brace`) is
/// captured with.
pub fn group_parser<'a>(delim: Delimiter) -> impl Parser<'a, &'a str, Vec<Token>, ParseError<'a>> + Clone {
    just(delim.open())
        .trivia_padded()
        .ignore_then(balanced_content(delim.close()))
        .then_ignore(just(delim.close()).trivia_padded())
        .try_map(|raw: &str, span| tokenize(raw).map_err(|msg| Rich::custom(span, msg)))
}

/// Parses a `( ... )` argument list into one token list per comma-separated
/// argument (each argument's own internal nested groups are unaffected --
/// only *top-level* commas, relative to this argument list, separate
/// arguments). This is the entry point a macro invocation's arguments are
/// captured with.
pub fn args_parser<'a>() -> impl Parser<'a, &'a str, Vec<Vec<Token>>, ParseError<'a>> + Clone {
    just('(')
        .trivia_padded()
        .ignore_then(balanced_content(')'))
        .then_ignore(just(')').trivia_padded())
        .try_map(|raw: &str, span| tokenize_args(raw).map_err(|msg| Rich::custom(span, msg)))
}

/// Assumes the caller has already consumed the opening delimiter; scans
/// forward (mirroring the depth-counting, escape-aware style of
/// `trivia::comment`, which this module intentionally does not try to reuse
/// as a sub-parser -- see the module-level rationale) tracking one shared
/// nesting depth across all three bracket kinds, until the matching close is
/// found, skipping over string/char literal contents and comments so a
/// bracket-looking character inside one never affects the count. A single
/// shared depth counter (rather than one per bracket kind) does not
/// specifically diagnose crossed delimiters like `{ ( }` -- true of any
/// single-depth-counter scan, and acceptable here since a well-formed source
/// never does this; `Scanner` (below) rebuilds real per-kind nested `Group`
/// structure from the already-known-balanced substring this returns.
/// Returns the raw source slice of everything between the open and its
/// matching close (not including either), leaving the cursor positioned
/// right before the close so the caller consumes it explicitly -- matching
/// how every other delimited-group parser in this grammar frames its own
/// `open ... close`.
fn balanced_content<'a>(close: char) -> impl Parser<'a, &'a str, &'a str, ParseError<'a>> + Clone {
    custom(move |inp: &mut InputRef<'a, '_, &'a str, ParseError<'a>>| {
        let start = inp.cursor();
        let mut depth = 1usize;
        loop {
            match inp.peek() {
                None => {
                    let span = inp.span_since(&start);
                    return Err(Rich::custom(span, format!("unterminated group, expected '{close}'")));
                }
                Some('(' | '[' | '{') => {
                    inp.next();
                    depth += 1;
                }
                Some(')' | ']' | '}') => {
                    if depth == 1 {
                        break;
                    }
                    inp.next();
                    depth -= 1;
                }
                Some('"') => skip_string(inp)?,
                Some('\'') => skip_char_literal(inp)?,
                Some('#') => skip_comment(inp)?,
                Some(_) => {
                    inp.next();
                }
            }
        }
        Ok(inp.slice_since(&start..))
    })
}

fn skip_string<'a>(inp: &mut InputRef<'a, '_, &'a str, ParseError<'a>>) -> Result<(), Rich<'a, char>> {
    let start = inp.cursor();
    inp.next(); // opening quote
    loop {
        match inp.peek() {
            None => {
                let span = inp.span_since(&start);
                return Err(Rich::custom(span, "unterminated string literal"));
            }
            Some('"') => {
                inp.next();
                return Ok(());
            }
            Some('\\') => {
                inp.next();
                skip_escape_body(inp);
            }
            Some(_) => {
                inp.next();
            }
        }
    }
}

fn skip_char_literal<'a>(inp: &mut InputRef<'a, '_, &'a str, ParseError<'a>>) -> Result<(), Rich<'a, char>> {
    let start = inp.cursor();
    inp.next(); // opening quote
    loop {
        match inp.peek() {
            None => {
                let span = inp.span_since(&start);
                return Err(Rich::custom(span, "unterminated char literal"));
            }
            Some('\'') => {
                inp.next();
                return Ok(());
            }
            Some('\\') => {
                inp.next();
                skip_escape_body(inp);
            }
            Some(_) => {
                inp.next();
            }
        }
    }
}

/// Skips whatever an escape's backslash was already consumed for -- one more
/// character in general, or a full `\u{XXXX}` run. Doesn't validate which
/// escape it is (that's the real string/char grammar's job at re-parse
/// time); this only needs to never mistake an escaped quote for the literal's
/// closing quote, which consuming "the backslash plus whatever follows it"
/// as one unit already guarantees.
fn skip_escape_body<'a>(inp: &mut InputRef<'a, '_, &'a str, ParseError<'a>>) {
    match inp.peek() {
        Some('u') => {
            inp.next();
            if inp.peek() == Some('{') {
                inp.next();
                while let Some(c) = inp.next() {
                    if c == '}' {
                        break;
                    }
                }
            }
        }
        Some(_) => {
            inp.next();
        }
        None => {}
    }
}

fn skip_comment<'a>(inp: &mut InputRef<'a, '_, &'a str, ParseError<'a>>) -> Result<(), Rich<'a, char>> {
    let start = inp.cursor();
    let mut hashes = 0usize;
    while inp.peek() == Some('#') {
        inp.next();
        hashes += 1;
    }
    if hashes == 1 {
        while let Some(c) = inp.peek() {
            if c == '\n' {
                break;
            }
            inp.next();
        }
        return Ok(());
    }
    loop {
        match inp.peek() {
            None => {
                let span = inp.span_since(&start);
                return Err(Rich::custom(span, format!("unterminated comment, expected {hashes} '#' to close it")));
            }
            Some('#') => {
                let mut run = 0usize;
                while inp.peek() == Some('#') {
                    inp.next();
                    run += 1;
                }
                if run == hashes {
                    return Ok(());
                }
            }
            Some(_) => {
                inp.next();
            }
        }
    }
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Tried longest-first so a shorter operator that happens to be a prefix of
/// a longer one (`".."` vs `"..."`) never wins by accident.
const MULTI_CHAR_PUNCT: &[&str] = &["...", "::", "=>", ":=", "==", "!=", "<=", ">=", "..", "++", "--"];

/// Every single-character punctuation symbol this grammar uses anywhere,
/// plus `!` (macro invocation's own `name!(...)` marker, otherwise only ever
/// seen as half of `!=`) -- grouping/comma-like structural characters
/// (`( ) [ ] { }`) are handled separately as `Group`s, never as `Punct`.
const SINGLE_CHAR_PUNCT: &str = "!%&*+,-./:;<=>";

/// A plain (non-chumsky) recursive-descent scanner over an already-extracted,
/// already-balanced substring (see `balanced_content` above) -- there is no
/// need to fight `InputRef`'s cursor-only API for genuine tokenization, since
/// arbitrary lookahead (needed for e.g. a numeric literal's radix prefix,
/// suffix, and maximal-munch punctuation) is trivial plain indexing once the
/// content is a known, fully-owned `&str`.
struct Scanner {
    chars: Vec<char>,
    pos: usize,
}

impl Scanner {
    fn new(input: &str) -> Self {
        Self { chars: input.chars().collect(), pos: 0 }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.chars.get(self.pos + offset).copied()
    }

    fn advance(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    fn slice_from(&self, start: usize) -> String {
        self.chars[start..self.pos].iter().collect()
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

    /// Mirrors `trivia::comment`'s hashes-counting rule. Reached only from
    /// `skip_trivia`, which has already confirmed the current character is
    /// `#`, so (unlike `trivia::comment`) there's no "not a comment at all"
    /// case to report here.
    fn skip_comment(&mut self) {
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
            return;
        }
        loop {
            match self.peek() {
                // The enclosing content was already proven balanced by
                // `balanced_content`, so an unterminated comment here would
                // mean that scan let a `#`-only "closing" run slip through
                // undetected -- defensively just stop rather than looping
                // forever; `tokenize`'s caller will fail to make sense of
                // whatever's left, which is an acceptable degraded error.
                None => return,
                Some('#') => {
                    let mut run = 0usize;
                    while self.peek() == Some('#') {
                        self.advance();
                        run += 1;
                    }
                    if run == hashes {
                        return;
                    }
                }
                Some(_) => {
                    self.advance();
                }
            }
        }
    }

    fn scan_token(&mut self) -> Result<Token, String> {
        self.skip_trivia();
        match self.peek() {
            None => Err("unexpected end of input".to_string()),
            Some('$') => self.scan_metavar(),
            Some('(') => self.scan_group(Delimiter::Paren),
            Some('[') => self.scan_group(Delimiter::Bracket),
            Some('{') => self.scan_group(Delimiter::Brace),
            Some(c @ (')' | ']' | '}')) => Err(format!("unexpected closing delimiter '{c}'")),
            Some('"') => Ok(Token::Literal(self.scan_string())),
            Some('\'') => Ok(Token::Literal(self.scan_char_literal())),
            Some(c) if c.is_ascii_digit() => Ok(Token::Literal(self.scan_number())),
            Some(c) if is_ident_start(c) => Ok(Token::Ident(self.scan_ident())),
            Some(_) => self.scan_punct(),
        }
    }

    fn scan_metavar(&mut self) -> Result<Token, String> {
        self.advance(); // '$'
        match self.peek() {
            Some(c) if is_ident_start(c) => Ok(Token::Metavar(self.scan_ident())),
            _ => Err("expected an identifier after '$'".to_string()),
        }
    }

    fn scan_ident(&mut self) -> String {
        let start = self.pos;
        self.advance();
        while self.peek().is_some_and(is_ident_continue) {
            self.advance();
        }
        self.slice_from(start)
    }

    fn scan_group(&mut self, delim: Delimiter) -> Result<Token, String> {
        self.advance(); // opening delimiter
        let mut tokens = Vec::new();
        loop {
            self.skip_trivia();
            if self.peek() == Some(delim.close()) {
                self.advance();
                return Ok(Token::Group(delim, tokens));
            }
            if self.peek().is_none() {
                return Err(format!("unterminated group, expected '{}'", delim.close()));
            }
            tokens.push(self.scan_token()?);
        }
    }

    fn scan_punct(&mut self) -> Result<Token, String> {
        for op in MULTI_CHAR_PUNCT {
            let op_chars: Vec<char> = op.chars().collect();
            if self.chars[self.pos..].starts_with(op_chars.as_slice()) {
                self.pos += op_chars.len();
                return Ok(Token::Punct((*op).to_string()));
            }
        }
        match self.peek() {
            Some(c) if SINGLE_CHAR_PUNCT.contains(c) => {
                self.advance();
                Ok(Token::Punct(c.to_string()))
            }
            Some(c) => Err(format!("unexpected character '{c}'")),
            None => Err("unexpected end of input".to_string()),
        }
    }

    /// Consumes a string literal's contents verbatim, including its
    /// delimiting quotes and any escapes -- mirrors `escape::escape_sequence`
    /// closely enough to never mistake `\"` for the closing quote (see
    /// `skip_escape_body`), without needing to validate which escape it is.
    fn scan_string(&mut self) -> String {
        let start = self.pos;
        self.advance(); // opening quote
        loop {
            match self.peek() {
                None => break,
                Some('"') => {
                    self.advance();
                    break;
                }
                Some('\\') => {
                    self.advance();
                    self.skip_escape_body();
                }
                Some(_) => {
                    self.advance();
                }
            }
        }
        self.slice_from(start)
    }

    fn scan_char_literal(&mut self) -> String {
        let start = self.pos;
        self.advance(); // opening quote
        loop {
            match self.peek() {
                None => break,
                Some('\'') => {
                    self.advance();
                    break;
                }
                Some('\\') => {
                    self.advance();
                    self.skip_escape_body();
                }
                Some(_) => {
                    self.advance();
                }
            }
        }
        self.slice_from(start)
    }

    fn skip_escape_body(&mut self) {
        match self.peek() {
            Some('u') => {
                self.advance();
                if self.peek() == Some('{') {
                    self.advance();
                    while let Some(c) = self.advance() {
                        if c == '}' {
                            break;
                        }
                    }
                }
            }
            Some(_) => {
                self.advance();
            }
            None => {}
        }
    }

    /// Mirrors `NumberExpr::parser`'s shape exactly (see
    /// `expression/number.rs`): a based prefix (`0x`/`0o`/`0b`) admits no
    /// whitespace before its digits, a decimal fractional part admits none
    /// before its `.`, and an explicit type suffix (`usize`/`isize`, or
    /// `i`/`u`/`f` + digits) admits none before it either -- so this has to
    /// replicate that adjacency precisely, not just "digits then letters":
    /// getting it wrong would split e.g. `5usize`/`0xFFu64` into two tokens,
    /// which `render`'s single-space join would then reintroduce whitespace
    /// into, breaking re-parsing (see this module's `render` doc comment).
    /// Doesn't validate the value (radix-correctness, suffix range, ...) --
    /// only needs to consume the same character shape as one atom; real
    /// validation happens when the rendered text is re-parsed for real.
    fn scan_number(&mut self) -> String {
        let start = self.pos;
        let radix = if self.peek() == Some('0') {
            match self.peek_at(1) {
                Some('x') => {
                    self.advance();
                    self.advance();
                    16
                }
                Some('o') => {
                    self.advance();
                    self.advance();
                    8
                }
                Some('b') => {
                    self.advance();
                    self.advance();
                    2
                }
                _ => 10,
            }
        } else {
            10
        };
        self.scan_radix_digits(radix);
        if radix == 10 && self.peek() == Some('.') && self.peek_at(1).is_some_and(|c| c.is_ascii_digit()) {
            self.advance();
            self.scan_radix_digits(10);
        }
        self.scan_number_suffix();
        self.slice_from(start)
    }

    fn scan_radix_digits(&mut self, radix: u32) {
        while let Some(c) = self.peek() {
            if c.is_digit(radix) || c == '_' {
                self.advance();
            } else {
                break;
            }
        }
    }

    fn scan_number_suffix(&mut self) {
        if self.try_consume_word("usize") || self.try_consume_word("isize") {
            return;
        }
        if matches!(self.peek(), Some('i' | 'u' | 'f')) && self.peek_at(1).is_some_and(|c| c.is_ascii_digit()) {
            self.advance();
            while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.advance();
            }
        }
    }

    /// Consumes `word` if it appears at the current position *and* isn't
    /// immediately followed by another identifier character (so `usizeish`
    /// doesn't wrongly match a `usize` suffix) -- mirrors `text::keyword`'s
    /// word-boundary check.
    fn try_consume_word(&mut self, word: &str) -> bool {
        let word_chars: Vec<char> = word.chars().collect();
        if self.chars[self.pos..].starts_with(word_chars.as_slice()) {
            let after = self.pos + word_chars.len();
            let boundary_ok = self.chars.get(after).is_none_or(|c| !is_ident_continue(*c));
            if boundary_ok {
                self.pos = after;
                return true;
            }
        }
        false
    }
}

/// Tokenizes a flat sequence (recursing into any nested groups by delimiter
/// kind, correctly this time -- unlike `balanced_content`, which only needed
/// to find where *its own* group ends). Used both directly (a macro
/// definition's `{ ... }` body, via `group_parser`) and once per argument
/// (via `tokenize_args`, below).
fn tokenize(input: &str) -> Result<Vec<Token>, String> {
    let mut scanner = Scanner::new(input);
    let mut tokens = Vec::new();
    loop {
        scanner.skip_trivia();
        if scanner.peek().is_none() {
            return Ok(tokens);
        }
        tokens.push(scanner.scan_token()?);
    }
}

/// Splits `input` (the raw text between a macro invocation's `(` and `)`) on
/// top-level commas and tokenizes each piece independently. "Top-level" here
/// falls out for free: `Scanner::scan_group` always consumes a nested
/// group's contents (including any commas inside it) as a single token
/// before ever returning control to this loop, so any `,` this loop's own
/// `peek()` sees is inherently outside every nested group already. A
/// trailing comma (`foo!(1, 2,)`) is tolerated; an empty argument (`foo!(1,
/// ,2)`, or `foo!(,)`) is rejected rather than silently producing an empty
/// token list for it.
fn tokenize_args(input: &str) -> Result<Vec<Vec<Token>>, String> {
    let mut scanner = Scanner::new(input);
    let mut args = Vec::new();
    scanner.skip_trivia();
    if scanner.peek().is_none() {
        return Ok(args);
    }
    loop {
        let mut tokens = Vec::new();
        loop {
            scanner.skip_trivia();
            match scanner.peek() {
                None | Some(',') => break,
                _ => tokens.push(scanner.scan_token()?),
            }
        }
        if tokens.is_empty() {
            return Err("empty macro argument".to_string());
        }
        args.push(tokens);
        scanner.skip_trivia();
        match scanner.peek() {
            Some(',') => {
                scanner.advance();
                scanner.skip_trivia();
                if scanner.peek().is_none() {
                    break; // trailing comma
                }
            }
            None => break,
            Some(c) => return Err(format!("unexpected character '{c}' in argument list")),
        }
    }
    Ok(args)
}
