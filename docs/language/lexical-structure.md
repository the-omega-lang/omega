# Lexical structure

This chapter defines how Omega source text is split into tokens before parsing.

## Source characters

The current implementation consumes UTF-8 source text. Identifiers themselves are ASCII-only; string and character contents may contain Unicode.

Whitespace separates tokens and otherwise has no semantic meaning except inside literals/comments.

## Identifiers

```ebnf
identifier-start    = "A".."Z" | "a".."z" | "_" ;
identifier-continue = identifier-start | "0".."9" ;
identifier          = identifier-start, { identifier-continue } ;
```

Identifiers are case-sensitive.

### Reserved keywords

These words are always tokenized as keywords:

```text
true false if else match extern import return
struct enum union spec
while loop for break continue defer
macro
```

### Contextual keywords

The following are lexed as ordinary identifiers and acquire special meaning only in grammar-defined positions:

```text
mut comp self reveal sizeof in exposed shared hidden
marker gap glue conform to primitive root super
expr type ident
asm reg const clobber
```

An implementation must therefore not globally reserve contextual keywords; they remain usable as identifiers where the grammar does not consume them contextually.

`asm`, `reg`, `const`, and `clobber` are recognized only as the head of an inline-assembly statement/descriptor; see [`inline-assembly.md`](inline-assembly.md). The raw text inside an `asm` body is not tokenized by this chapter's rules at all — see that chapter for its opaque capture behavior.

## Comments

A single hash begins a line comment:

```omega
# comment through end of line
```

A run of **N hashes where N >= 2** opens a multiline comment. It is closed by a run of exactly N hashes:

```omega
##
multiline comment
##

#### embedded ### runs do not close this ####
```

An unterminated multiline comment is a lexical error.

Comments are discarded before parsing except for their contribution to source spans/diagnostics.

## String literals

Ordinary strings use double quotes:

```omega
"hello\nworld"
```

Supported escapes are:

```text
\n   newline
\t   tab
\r   carriage return
\0   NUL
\\   backslash
\"   double quote
\'   single quote
\u{HEX} Unicode scalar value, 1 through 6 hexadecimal digits
```

A decoded `\u{...}` value must be a valid Unicode scalar value.

### Multiline strings

A delimiter run of at least three quote characters starts a multiline string. A matching quote run closes it. Multiline content is raw/verbatim rather than ordinary escape-decoded content.

The current lexer diagnoses an even-sized multiline delimiter run; portable Omega source should use an odd-sized delimiter run.

## Byte strings

Byte strings use a leading `b` directly before an ordinary string literal:

```omega
b"raw bytes"
```

They use the same source-level escape decoding shape as ordinary strings. A byte-string literal does **not** imply a trailing NUL byte.

## Character literals

A character literal is a single character or one supported escape between single quotes:

```omega
'A'
'é'
'\n'
'\u{03A9}'
```

A character literal containing zero or multiple decoded characters is invalid.

## Boolean literals

```text
true
false
```

These are keyword tokens, not identifiers.

## Numeric literals

Integer bases:

```text
123       decimal
0xFF      hexadecimal
0o755     octal
0b1010    binary
```

Underscores may appear within the digit run and do not contribute to the value:

```text
1_000_000
0xFF_FF
```

A decimal literal may contain a fractional part only when `.` is followed by a decimal digit:

```text
1.0
0.125f32
```

This rule prevents the range/member punctuation following an integer from being swallowed as a decimal point.

### Numeric suffixes

A numeric literal may carry one of these suffix shapes:

```text
usize
isize
i<decimal-digits>
u<decimal-digits>
f<decimal-digits>
```

Examples:

```omega
7u32
10i64
3.5f32
8usize
```

Lexing recognizes the suffix shape; semantic analysis decides whether the resulting type is a supported primitive and whether the value fits.

Unsuffixed literal defaulting and contextual narrowing are specified in [`types-and-primitives.md`](types-and-primitives.md) and [`control-flow-and-operators.md`](control-flow-and-operators.md).

## Punctuation and compound tokens

Omega recognizes these compound punctuation tokens greedily:

```text
...  ::  =>  :=
==   !=  <=  >=
..=  ..<  ..
++   --   &&  ||  <<  >>
+=   -=   *=  /=  %=  &=  |=  ^=
<<=  >>=
```

Single-character punctuation/operators include:

```text
$ % & * + , - . / : ; < = > | ^ ~ ! @ ?
( ) [ ] { }
```

`...` is distinct from range syntax and is used only in the syntactic contexts that permit variadics/repetition. `..`, `..=`, and `..<` participate in ranges/slices/patterns as specified elsewhere.

When several tokenizations are possible, the lexer chooses the longest recognized punctuation token. Parser contexts that need nested closing `>` tokens must interpret the resulting token stream according to the type/generic grammar.
