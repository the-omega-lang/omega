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
     | foreign-binding
     | foreign-function-item
     | foreign-block
     | function-definition
     | struct-declaration
     | union-declaration
     | marker-declaration
     | enum-declaration
     | spec-declaration
     | conformance
     | primitive-block
     | gap-declaration
     | glue-block
     | macro-definition
     | macro-invocation, ";"
     | alias-declaration
     | global-declaration, ";"
     | global-binding, ";" ;

global-declaration = [ visibility ], [ "mut" ], identifier, ":", type, [ "=", expression ] ;
global-binding = [ visibility ], [ "mut" ], [ "comp" ], identifier, ":=", expression ;
```

Annotations are syntactically accepted only before supported item kinds; semantic applicability is defined in [`annotations-and-sizeof.md`](annotations-and-sizeof.md).

Top-level source may contain imports, foreign bindings/functions/blocks, aggregate/spec/gap/glue/conformance/primitive declarations, macro definitions/invocations, global bindings/declarations, and function definitions. Local aggregate/spec declarations -- and local `foreign` items of any shape -- are not permitted inside function bodies.

## Visibility

```ebnf
visibility = "exposed" | "shared" | "hidden" ;
```

Omitted visibility means hidden, except on a spec member, where it means the declaring spec's own visibility. Visibility legality varies by item/member; see [`visibility.md`](visibility.md).

## Paths

```ebnf
path-anchor  = "root", "::" | "self", "::" | { "super", "::" } ;
path         = [ path-anchor ], identifier, { "::", identifier } ;
type-path    = path, [ generic-arguments ] ;

generic-arguments = "<", type, { ",", type }, ">" ;
```

`path-anchor` is a general part of `path`, legal wherever a path is legal --
a type position (including nested inside pointer, array, or
generic-argument syntax), an expression, a function type, an alias target, an
import, or a macro body -- not import-only syntax. `root`, `self`, and
`super` are contextual: only a leading anchor form followed by `::` is
navigation; elsewhere (including the final segment of an unanchored path)
they remain ordinary identifiers. Anchor meaning is specified in
[`modules-and-imports.md`](modules-and-imports.md).

Expression-path generic arguments are committed purely syntactically, by what follows the closing `>`: a path continuation (`Type<T>::member`), a generic struct-literal brace (`Type<T> { ... }`), a call suffix (`f<T>(...)`), or an expression boundary (`;`, `,`, `)`, `]`, `}`, end of input), as in `&foo<T>;` or `[foo<T>, bar<T>]`. Anything else after `>` starts a fresh operand, so the attempt is rolled back and `a < b > c` stays a chained comparison. Commitment never depends on name or type resolution: `a<b>(c)` is a generic call whose failure is a generic/name/callee error, never a re-reading as comparisons. Omega has no turbofish spelling; `::<...>` is not syntax.

## Imports

```ebnf
import       = "import", [ "reveal" ], path, import-tail, ";" ;

import-tail  = [ "as", identifier ]
             | "::", import-group ;

import-group = "{", import-entry, { ",", import-entry }, [ "," ], "}" ;

import-entry = [ "reveal" ],
               ( "self", [ "as", identifier ]
               | identifier, { "::", identifier }, import-tail ) ;
```

An import's `path` is an ordinary anchored or unanchored path; an unanchored
import is top-level-by-default, unlike an unanchored path elsewhere. A group
attaches to that path, so a group needs at least one written segment:
`self::{ ... }`, `root::{ ... }`, and `super::{ ... }` are not import
prefixes. `as` renames only a terminal binding, and `self` is a terminal
group entry naming the enclosing prefix; neither may be followed by further
path segments or by a group. Module/import meaning is specified in [`modules-and-imports.md`](modules-and-imports.md).

## Foreign items

```ebnf
foreign-binding = [ visibility ], "foreign", identifier, ":", type, ";" ;

foreign-function-item = [ visibility ], "foreign", [ calling-convention ],
                        identifier, [ generic-parameters ],
                        "(", [ parameter-list-rest ], ")", "=>", type,
                        ( ";" | code-block ) ;

foreign-block = "foreign", calling-convention, "{", { foreign-block-entry }, "}" ;

foreign-block-entry = [ visibility ], identifier,
                      ( ":", type, ";"
                      | "(", [ parameter-list-rest ], ")", "=>", type, ( ";" | code-block ) ) ;
```

`foreign-binding`'s `identifier, ":"` and `foreign-function-item`'s `identifier, "("`/`"<"` are unambiguous on the token right after the name. `foreign(cc) name : Type;` (a `calling-convention` directly on a binding) is rejected -- see [`foreign-function-interface.md`](foreign-function-interface.md) for the exact rule and the equivalent unambiguous spelling. Inside a `foreign-block`, `calling-convention` is never written again; the block's own applies to each direct function-signature entry (not to a `":", type` entry), and blocks do not nest.

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
     | foreign-function-type
     | spec-type
     | anonymous-enum-type
     | type-path ;

pointer-type            = "*", [ "mut" ], type ;
fixed-array-type        = "[", decimal-integer, "]", type ;
inferred-array-type     = "[", "]", type ;
unknown-size-array-type = "[", "?", "]", type ;

spec-type = "spec", type-path, { "+", type-path } ;

anonymous-enum-type = "enum", type, { "|", type } ;
```

`spec-type` parses one static conjunction of members; there is no `spec *...` prefix-pointer spelling. `pointer-type = "*", ["mut"], type` already covers a dynamic spec object structurally: `*spec A + B` and `*mut spec A + B` are ordinary `pointer-type`s whose `type` is a `spec-type`. Semantic type resolution recognizes that specific immediate combination and turns it into a dynamic spec-object type; the grammar itself does not distinguish "static" from "dynamic" spec types. See [`specs-and-conformance.md`](specs-and-conformance.md).

`anonymous-enum-type` is a structural sum type whose variants are its member
types; one member (`enum A`) is legal. Each member is a full `type`, and `|`
separates members at this production only -- so `enum (i32) => bool | i32` has
the two members `(i32) => bool` and `i32`, and the member list ends at the first
token that cannot continue a type (`,`, `>`, `)`, `;`, `{`, ...). A member that
is itself an `anonymous-enum-type` therefore consumes the rest of the list:
`enum A | enum B | C` has the two members `A` and `enum B | C`. `enum` is
already a keyword, so this never collides with `type-path`. Member identity,
canonical ordering, tags, and conversions are in
[`enums-and-pattern-matching.md`](enums-and-pattern-matching.md).

`[]T` is syntactically a type form used where array length is inferred; its legal semantic positions are restricted by [`types-and-primitives.md`](types-and-primitives.md). Slice values use pointer forms such as `*[]T`/`*mut []T`.

### Function types and receivers

```ebnf
function-type = "(", [ function-type-parameters ], ")", "=>", type ;

foreign-function-type = "foreign", calling-convention, function-type ;

calling-convention = "(", identifier, ")" ;

function-type-parameters = [ receiver, [ "," ] ], parameter-list-rest
                         | receiver
                         | parameter-list-rest ;

parameter-list-rest = function-type-parameter, { ",", function-type-parameter },
                      [ ",", "..." ] ;

function-type-parameter = [ identifier, ":" ], type ;

parameter = identifier, ":", type ;

receiver = "self"
         | "mut", "self"
         | "*", "self"
         | "*", "mut", "self" ;
```

A `function-type-parameter`'s `identifier` is optional descriptive metadata and is not part of the type; `parameter` (used by declarations, spec/gap/glue members, and every other binding position) still requires it. A leading `*` begins a `receiver` only in the exact `"*", "self"` / `"*", "mut", "self"` spellings, so `(*Thing) => void` is a parameter whose type is a pointer. `function-type` always denotes the implicit Omega calling convention; `foreign-function-type` names an explicit non-Omega one (currently `c` or `sysv64`). Bare `"foreign", function-type` (no `calling-convention`) is not a valid type -- `calling-convention` is mandatory in `foreign-function-type` and immediately follows the keyword, which is also what keeps this production unambiguous with a plain `function-type`'s own leading `"("`. `...` is legal in `parameter-list-rest` only where the enclosing function type's convention supports variadics; an ordinary Omega-convention function is never variadic. See [`functions.md`](functions.md) and [`foreign-function-interface.md`](foreign-function-interface.md).

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

spec-member = [ visibility ], identifier,
              "(", [ spec-parameters ], ")",
              "=>", type,
              ( ";" | code-block ) ;

spec-parameters = function-parameters, [ ",", "..." ]
                | receiver, ",", "..."
                | "..." ;
```

A spec member does not introduce its own generic-parameter list. A body supplies a default implementation. A spec declaration never carries a dependency/composition list: `spec X : A, B { ... }` and `spec X = A + B;` are both invalid Omega syntax; express requirements as generic bounds and/or blanket conformances, and spell a conjunction directly at the type where it is needed (`spec A + B`, `*spec A + B`), optionally giving that conjunction a name with an `alias-declaration`.

A spec member's visibility modifier, when given, must not exceed the declaring spec's own visibility; when omitted, it defaults to the spec's own visibility rather than to hidden (see [`visibility.md`](visibility.md)).

## Conformance

```ebnf
conformance = "meet", [ generic-parameters ], spec-target, "for", conforming-target,
              "{", { conformance-method }, "}" ;

spec-target       = type ;
conforming-target = type ;

conformance-method = identifier,
                     "(", [ function-parameters ], ")",
                     "=>", type,
                     code-block ;
```

The spec being met is written first and the conforming target follows `for`. Explicit visibility modifiers on a conformance block or its methods are not part of the conformance grammar; visibility is inherited according to conformance rules. See [`specs-and-conformance.md`](specs-and-conformance.md).

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

## Aliases

```ebnf
alias-declaration = [ visibility ], "alias", identifier,
                    [ "<", generic-parameter-list, ">" ], "=", type, ";" ;
```

An alias is a top-level item only; it is not a statement, takes no annotations,
and its right-hand side is type syntax, never an expression. A right-hand side
that is a bare `type-path` may name any namespace (module, type, spec, function
or overload set, macro, or another alias); the parser does not classify it. See
[`aliases.md`](aliases.md).

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
          | defer-statement
          | while-statement
          | loop-statement
          | for-statement
          | asm-statement
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

## Inline assembly

```ebnf
asm-statement = "asm", "(", [ asm-descriptor, { ",", asm-descriptor }, [ "," ] ], ")",
                "=>", "{", asm-body, "}" ;

asm-descriptor = "reg", "(", expression, [ ",", string-literal ], ")"
                | "const", "(", identifier, ")"
                | "clobber", "(", string-literal, ")" ;
```

`asm-body` is opaque backend assembly text, not Omega syntax; it is delimited only by architecture-neutral brace balancing. See [`inline-assembly.md`](inline-assembly.md).

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
               | "(", [ argument-list ], ")"
               | "?" ;
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

Calls and array literals do **not** accept a trailing comma in the current grammar. Expression-path generic arguments are accepted only when the token after the closing `>` continues the path, opens a generic struct literal, applies a call, or ends the expression; see the path rule above.

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

Patterns are value patterns or ranges; bare `..` is a catch-all. A `pattern` that parses completely as a `type` up to its `=>` may also be read as a type pattern, which is how an anonymous enum's members are matched; that reading is selected only when the scrutinee is an anonymous enum, so `Enum::Variant`, literal, range, and constant-value patterns keep their ordinary meaning everywhere else. Enum/refinement semantics are specified in [`enums-and-pattern-matching.md`](enums-and-pattern-matching.md).

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
postfix:          call, index/slice, field/member access, try (?)
```

Assignments require an assignable place. Comparison operators are non-associative, so chained comparison syntax is rejected.

Operator typing, short-circuiting, pointer arithmetic, literal inference and assignment semantics are specified in [`control-flow-and-operators.md`](control-flow-and-operators.md), [`types-and-primitives.md`](types-and-primitives.md), and [`bindings-and-mutability.md`](bindings-and-mutability.md).
