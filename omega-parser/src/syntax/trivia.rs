use crate::syntax::ParseError;
use chumsky::{error::Rich, input::InputRef, prelude::*};

/// Matches a single comment: `#` starts a single-line comment (consumed to
/// end of line, or EOF); two or more repeated `#`s start a multi-line
/// comment, which must be closed by a run of *exactly* the same number of
/// `#`s (a longer or shorter run doesn't close it, and is just more comment
/// content). An unterminated multi-line comment is a parse error rather than
/// silently consuming the rest of the file.
///
/// Written with `custom` rather than the `configure`/`ignore_with_ctx`
/// delimiter-counting combinators `expression::string` uses for its quote
/// counting (that implementation's own comments flag it as fragile) --
/// counting leading `#`s and then scanning for a matching-count run is a
/// direct imperative loop this way, with no combinator fighting involved.
fn comment<'a>() -> impl Parser<'a, &'a str, (), ParseError<'a>> + Clone {
    custom(|inp: &mut InputRef<'a, '_, &'a str, ParseError<'a>>| {
        let start = inp.cursor();

        let mut hashes = 0usize;
        while inp.peek() == Some('#') {
            inp.next();
            hashes += 1;
        }

        if hashes == 0 {
            let span = inp.span_since(&start);
            return Err(Rich::custom(span, "expected a comment"));
        }

        if hashes == 1 {
            // Single-line: consume up to (not including) the newline, or EOF.
            while let Some(c) = inp.peek() {
                if c == '\n' {
                    break;
                }
                inp.next();
            }
            return Ok(());
        }

        // Multi-line: scan for a run of exactly `hashes` `#`s. A run of a
        // different length doesn't close it and is just more content.
        loop {
            match inp.peek() {
                None => {
                    let span = inp.span_since(&start);
                    return Err(Rich::custom(
                        span,
                        format!("unterminated comment, expected {hashes} '#' to close it"),
                    ));
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
    })
}

/// Whitespace or comments, any number of times -- the "ignored between
/// tokens" trivia for the whole grammar.
fn trivia<'a>() -> impl Parser<'a, &'a str, (), ParseError<'a>> + Clone {
    choice((any().filter(|c: &char| c.is_whitespace()).ignored(), comment()))
        .repeated()
        .ignored()
}

/// Extends any parser with `.trivia_padded()`, the trivia-aware counterpart
/// to chumsky's own `.padded()` (whitespace only) -- named distinctly since
/// having both `TriviaExt` and chumsky's `Parser` provide a method called
/// `padded` in the same scope would be ambiguous.
pub trait TriviaExt<'a, O>: Parser<'a, &'a str, O, ParseError<'a>> + Sized {
    fn trivia_padded(self) -> impl Parser<'a, &'a str, O, ParseError<'a>> + Clone
    where
        Self: Clone;
}

impl<'a, O, P> TriviaExt<'a, O> for P
where
    P: Parser<'a, &'a str, O, ParseError<'a>>,
{
    fn trivia_padded(self) -> impl Parser<'a, &'a str, O, ParseError<'a>> + Clone
    where
        Self: Clone,
    {
        self.padded_by(trivia())
    }
}
