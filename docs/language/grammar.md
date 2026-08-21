# Grammar

This chapter gives a compact implementation grammar for current Omega. It should be read together with [`lexical-structure.md`](lexical-structure.md) and the semantic chapters linked below.

The notation is EBNF-like:

- `{ X }` — zero or more
- `[ X ]` — optional
- `X | Y` — alternatives
- quoted text — literal token/keyword
- names such as `expression` refer to productions or semantic token classes

Whitespace/comments are omitted.

## Compilation unit

```ebnf
module = { [ annotation-list ], item } ;

item = import
     | extern-declaration
     | function-definition
     | struct-declaration
     | union-declaration
     | marker-declaration
     | enum-declaration
     | spec-declaration
     | spec-alias
     | conformance
     | primitive-block
     | gap-declaration
     | glue-block
     | macro-definition
     | macro-invocation, ";"
     | global-declaration, ";"
     | global-binding, ";" ;

global-declaration = [ visibility ], [ "mut" ], identifier, ":", type, [ "=", expression ] ;
global-binding = [ visibility ], [ "mut" ], [ "comp" ], identifier, ":=", expression ;
```

Annotations are syntactically accepted only before supported item kinds; semantic applicability is defined in [`annotations-and-sizeof.md`](annotations-and-sizeof.md).

Top-level source may contain imports, extern declarations, aggregate/spec/gap/glue/conformance/primitive declarations, macro definitions/invocations, global bindings/declarations, and function definitions. Local aggregate/spec declarations are not permitted inside function bodies.

## Visibility

```ebnf
visibility = "exposed" | "shared" ;
```

Omitted visibility means hidden. Visibility legality varies by item/member; see [`visibility.md`](visibility.md).

## Paths

```ebnf
path         = identifier, { "::", identifier } ;
rooted-path  = [ "root", "::" | "extern", "::" ], path ;
type-path    = path, [ generic-arguments ] ;

generic-arguments = "<", type, { ",", type }, ">" ;
```

Expression-path generic arguments are only syntactically committed where the following syntax makes them unambiguous (for example `Type<T>::member` or `Type<T> { ... }`). Ordinary function calls rely on inference rather than Rust-style turbofish syntax.

## Imports

```ebnf
import = "import", [ "reveal" ], import-root, ";" ;
import-root = [ "extern", "::" | "root", "::" ], path ;
```

Module/import meaning is specified in [`modules-and-imports.md`](modules-and-imports.md).

## Extern declarations

```ebnf
extern-declaration = [ visibility ], "extern", identifier, ":", function-type, ";" ;
```

See [`foreign-function-interface.md`](foreign-function-interface.md).

## Generic parameters and bounds

```ebnf
generic-parameters = "<", generic-parameter, { ",", generic-parameter }, ">" ;

generic-parameter = identifier,
                    [ ":", spec-bound, { "+", spec-bound } ],
                    [ "=", type ] ;

spec-bound = type-path ;
```

Once a generic parameter has a default, every later generic parameter in that list must also have a default. Full semantics are in [`generics.md`](generics.md).

## Types

```ebnf
type = pointer-type
     | fixed-array-type
     | inferred-array-type
     | unknown-size-array-type
     | function-type
     | spec-type
     | type-path ;

pointer-type            = "*", [ "mut" ], type ;
fixed-array-type        = "[", decimal-integer, "]", type ;
inferred-array-type     = "[", "]", type ;
unknown-size-array-type = "[", "?", "]", type ;

spec-type = "spec", [ "*", [ "mut" ] ], type-path ;
```

`[]T` is syntactically a type form used where array length is inferred; its legal semantic positions are restricted by [`types-and-primitives.md`](types-and-primitives.md). Slice values use pointer forms such as `*[]T`/`*mut []T`.

### Function types and receivers

```ebnf
function-type = "(", [ function-type-parameters ], ")", "=>", type ;

function-type-parameters = [ receiver, [ "," ] ], parameter-list-rest
                         | receiver
                         | parameter-list-rest ;

parameter-list-rest = parameter, { ",", parameter }, [ ",", "..." ] ;

parameter = identifier, ":", type ;

receiver = "self"
         | "mut", "self"
         | "*", "self"
         | "*", "mut", "self" ;
```

Variadic `...` is restricted to C-interoperability function types/calls; pure Omega functions are not variadic. See [`functions.md`](functions.md) and [`foreign-function-interface.md`](foreign-function-interface.md).

## Functions

```ebnf
function-definition = [ visibility ], identifier,
                      [ generic-parameters ],
                      "(", [ function-parameters ], ")",
                      "=>", type,
                      code-block ;

function-parameters = [ receiver, [ "," ] ], parameter, { ",", parameter }
                    | receiver
                    | parameter, { ",", parameter } ;
```

There is deliberately no `fn` keyword and the return arrow is `=>`.

## Structs, unions, and markers

```ebnf
struct-declaration = [ visibility ], "struct", identifier,
                     [ generic-parameters ],
                     "{", { field }, { function-definition }, "}" ;

union-declaration = [ visibility ], "union", identifier,
                    [ generic-parameters ],
                    "{", { field }, { function-definition }, "}" ;

marker-declaration = [ visibility ], "marker", identifier,
                     [ generic-parameters ],
                     "{", { function-definition }, "}" ;

field = [ visibility ], identifier, ":", type, ";" ;
```

Fields precede methods. `marker` has no field grammar. Aggregate semantics are specified in [`structs-and-unions.md`](structs-and-unions.md) and [`marker-types.md`](marker-types.md).

## Enums

Current enums support optional generic parameters, optional header fields, optional explicit tag declaration, shared dynamic fields, variant fields, and methods.

```ebnf
enum-declaration = [ visibility ], "enum", identifier,
                   [ generic-parameters ],
                   [ "(", enum-header, ")" ],
                   "{",
                       { field },
                       [ enum-variants, [ ";", { function-definition } ] ],
                   "}" ;

enum-header = enum-header-entry, { ",", enum-header-entry } ;
enum-header-entry = [ visibility ], identifier, ":", type ;

enum-variants = enum-variant, { enum-variant-separator, enum-variant }, [ "," ] ;
enum-variant-separator = "," | /* omitted after a variant field body */ ;

enum-variant = identifier,
               [ "(", expression, { ",", expression }, ")" ],
               [ "{", { field }, "}" ] ;
```

The distinguished header field named `tag` gives an explicit tag type. Variant argument lists supply compile-time values for the enum header positionally; they are expressions, not declarations. A comma normally separates variants, but a variant with its own `{ field... }` body may be followed directly by the next variant. A `;` ends the variant list before methods. Exact field categories, construction, and narrowing are in [`enums-and-pattern-matching.md`](enums-and-pattern-matching.md).

## Specs

```ebnf
spec-declaration = [ visibility ], "spec", identifier,
                   [ generic-parameters ],
                   "{", { spec-member }, "}" ;

spec-alias = [ visibility ], "spec", identifier,
             [ generic-parameters ],
             "=", type, { "+", type }, ";" ;

spec-member = identifier,
              "(", [ spec-parameters ], ")",
              "=>", type,
              ( ";" | code-block ) ;

spec-parameters = function-parameters, [ ",", "..." ]
                | receiver, ",", "..."
                | "..." ;
```

A spec member does not introduce its own generic-parameter list. A body supplies a default implementation. `spec X : A, B { ... }` is not valid Omega syntax; express requirements as generic bounds and/or blanket conformances.

## Conformance

```ebnf
conformance = "conform", [ generic-parameters ], type, "to", type,
              "{", { conformance-method }, "}" ;

conformance-method = identifier,
                     "(", [ function-parameters ], ")",
                     "=>", type,
                     code-block ;
```

Explicit visibility modifiers on a `conform` block or its methods are not part of the conformance grammar; visibility is inherited according to conformance rules. See [`specs-and-conformance.md`](specs-and-conformance.md).

## Primitive extension blocks

```ebnf
primitive-block = "primitive", [ generic-parameters ], type,
                  "{", { function-definition }, "}" ;
```

The block itself has no visibility modifier; methods may use their normal member visibility. Semantics are in [`specs-and-conformance.md`](specs-and-conformance.md) and [`types-and-primitives.md`](types-and-primitives.md).

## Gaps and glue

```ebnf
gap-declaration = "gap", identifier,
                  "{", { gap-function }, "}" ;

gap-function = identifier,
               "(", [ parameter, { ",", parameter } ], ")",
               "=>", type, ";" ;

glue-block = "glue", path,
             "{", { glue-function }, "}" ;

glue-function = identifier,
                "(", [ parameter, { ",", parameter } ], ")",
                "=>", type,
                code-block ;
```

Gap/glue functions do not have receiver or generic syntax. See [`gaps-and-glue.md`](gaps-and-glue.md).

## Macros

```ebnf
macro-definition = "macro", identifier, "(", [ macro-parameter-list ], ")",
                   "=>", token-tree ;

macro-invocation = identifier, "$", "(", token-tree-content, ")" ;
```

Macro parameter kinds and repetition are specified in [`macros.md`](macros.md); macro bodies are token trees rather than ordinary expression grammar.

## Annotations

```ebnf
annotation-list = { annotation } ;
annotation = "@", identifier, [ "(", [ annotation-args ], ")" ] ;
annotation-args = annotation-arg, { ",", annotation-arg } ;
annotation-arg = identifier | identifier, "=", annotation-value ;
annotation-value = decimal-integer | string-literal | "sizeof", "<", type, ">" ;
```

Exact recognized names and applicability are in [`annotations-and-sizeof.md`](annotations-and-sizeof.md).

## Statements and blocks

```ebnf
code-block = "{", { statement }, [ expression ], "}" ;

statement = local-declaration, ";"
          | inferred-binding, ";"
          | return-statement, ";"
          | break-statement, ";"
          | continue-statement, ";"
          | extern-declaration
          | defer-statement
          | while-statement
          | loop-statement
          | for-statement
          | expression-statement ;

local-declaration = [ "mut" ], identifier, ":", type, [ "=", expression ] ;
inferred-binding  = [ "mut" ], [ "comp" ], identifier, ":=", expression ;

return-statement   = "return", expression ;
break-statement    = "break" ;
continue-statement = "continue" ;

defer-statement = "defer", statement-or-block ;
```

A bare `return;` is not accepted by the current grammar; this limitation is tracked in [`../issues/known-issues.md`](../issues/known-issues.md).

A block-shaped expression used directly as a statement (`{...}`, `if`, `match`) does not require a trailing semicolon. Other expression statements do.

The optional final expression of a code block is its value.

## Loops

```ebnf
while-statement = "while", expression-no-leading-struct-literal, code-block ;
loop-statement  = "loop", code-block ;

for-statement = c-for | for-in ;

c-for = "for", [ for-init ], ";", [ expression ], ";", [ expression ], code-block ;
for-init = local-declaration | inferred-binding | expression ;

for-in = "for", [ "mut" ], identifier, [ ":", type ], "in", expression-no-leading-struct-literal, code-block ;
```

The parser can represent an omitted classic-`for` condition for recovery, but semantic analysis requires one. Thus `for ;; { ... }` is not a valid Omega program; use `loop { ... }` for an unconditional loop.

Condition-bearing contexts syntactically restrict a leading struct literal so that `if flag { ... }` cannot be misread as a literal `flag { ... }`. See [`control-flow-and-operators.md`](control-flow-and-operators.md) and [`iteration-and-ranges.md`](iteration-and-ranges.md).

## Expressions

Omega expressions use the following precedence structure. Assignment and range formation are right/outer layers; ordinary binary tiers are left-associative except comparisons, which are non-associative.

```ebnf
expression = range-expression | assignment ;

range-expression = [ assignment ], ".."
                 | [ assignment ], ( "..<" | "..=" ), assignment ;

assignment = logical-or, [ assignment-op, expression ] ;
assignment-op = "=" | "+=" | "-=" | "*=" | "/=" | "%="
              | "&=" | "|=" | "^=" | "<<=" | ">>=" ;

logical-or  = logical-and, { "||", logical-and } ;
logical-and = comparison, { "&&", comparison } ;
comparison  = bitwise-or, [ comparison-op, bitwise-or ] ;
comparison-op = "==" | "!=" | "<" | ">" | "<=" | ">=" ;

bitwise-or  = bitwise-xor, { "|", bitwise-xor } ;
bitwise-xor = bitwise-and, { "^", bitwise-and } ;
bitwise-and = shift, { "&", shift } ;
shift       = additive, { ( "<<" | ">>" ), additive } ;
additive    = multiplicative, { ( "+" | "-" ), multiplicative } ;
multiplicative = unary, { ( "*" | "/" | "%" ), unary } ;

unary = "*", unary
      | "&", unary
      | "&", "mut", unary
      | "-", unary
      | "~", unary
      | "!", unary
      | "++", unary
      | "--", unary
      | "reveal", unary
      | "comp", unary
      | "<", type, ">", unary
      | qualified-spec-member
      | postfix ;

qualified-spec-member = "<", type, ":", type, ">", "::", identifier, { postfix-suffix } ;

postfix = primary, { postfix-suffix } ;
postfix-suffix = ".", identifier
               | "[", ( expression | range-expression ), "]"
               | "(", [ argument-list ], ")" ;
argument-list = expression, { ",", expression } ;

primary = literal
        | expression-path
        | struct-literal
        | array-literal
        | "(", expression, ")"
        | code-block
        | if-expression
        | match-expression
        | macro-invocation
        | "sizeof", "<", type, ">" ;

array-literal = "[", [ expression, { ",", expression } ], "]" ;
struct-literal = expression-path, "{", { field-initializer }, "}" ;
field-initializer = identifier, "=", expression, ";" ;
```

Calls and array literals do **not** accept a trailing comma in the current grammar. Expression-path generic arguments are accepted only when syntax after the closing `>` makes the generic reading unambiguous (notably a following `::` segment or a struct literal); ordinary generic function calls infer type arguments rather than using turbofish syntax.

In `if`, `while`, `match` scrutinees, and `for` headers, an unparenthesized leading struct literal is syntactically restricted so the following `{` can unambiguously begin the control-flow body. Parentheses can make the struct literal explicit when needed.

### `if`

```ebnf
if-expression = "if", expression-no-leading-struct-literal, code-block,
                { "else", "if", expression-no-leading-struct-literal, code-block },
                [ "else", code-block ] ;
```

### `match`

```ebnf
match-expression = "match", expression-no-leading-struct-literal,
                   "{", match-arm, { ",", match-arm }, [ "," ], "}",
                   [ "else", code-block ] ;

match-arm = pattern, "=>", expression ;
```

Patterns are value patterns or ranges; bare `..` is a catch-all. Enum/refinement semantics are specified in [`enums-and-pattern-matching.md`](enums-and-pattern-matching.md).

### Range syntax

```ebnf
range         = open-range | bounded-range ;
open-range    = [ expression ], ".." ;
bounded-range = [ expression ], ( "..<" | "..=" ), expression ;
```

`..` specifically denotes an open-ended range and therefore cannot be followed by an end expression. `..<` and `..=` always require an explicit end; their start may be omitted when the surrounding semantic context can supply/infer it. The same range syntax is used by ordinary range expressions, slices, and range patterns. See [`iteration-and-ranges.md`](iteration-and-ranges.md), [`strings-casts-arrays-and-slices.md`](strings-casts-arrays-and-slices.md), and [`enums-and-pattern-matching.md`](enums-and-pattern-matching.md).

## Operators and precedence

From loosest to tightest:

```text
assignment:       = += -= *= /= %= &= |= ^= <<= >>=
logical or:       ||
logical and:      &&
comparison:       == != < > <= >=        (non-associative)
bitwise or:       |
bitwise xor:      ^
bitwise and:      &
shift:            << >>
additive:         + -
multiplicative:   * / %
unary:            - ! * & &mut ~ ++ -- reveal comp
cast:             <Type>expression
postfix:          call, index/slice, field/member access
```

Assignments require an assignable place. Comparison operators are non-associative, so chained comparison syntax is rejected.

Operator typing, short-circuiting, pointer arithmetic, literal inference and assignment semantics are specified in [`control-flow-and-operators.md`](control-flow-and-operators.md), [`types-and-primitives.md`](types-and-primitives.md), and [`bindings-and-mutability.md`](bindings-and-mutability.md).
