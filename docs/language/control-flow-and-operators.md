# Control flow and operators

This chapter is normative for current Omega language behavior. Known implementation limitations are tracked under [`../issues/`](../issues/).

## `if` expressions

```omega
result := if x > 0 { "positive" }
          else if x < 0 { "negative" }
          else { "zero" };
```

`if` is an expression. Its condition must be `bool`. When an `if` is used as a value, its value-producing branches must be type-compatible.

When the `if` sits in a position with an expected type — an annotated declaration, an argument, a `return`, a field or element — that expected type is what every value-producing branch is checked against, so each branch may convert to it exactly as a standalone expression in that position would. Otherwise branch typing is source-order anchored: the first branch establishes the initial result type/literal expectation and later branches are checked against it. For example, `if true { 10 } else { 20u64 }` is invalid because the first unsuffixed `10` defaults to `i32`; the later explicit `u64` does not retroactively choose the first branch's type.

An untyped branch join never manufactures a type of its own. Branches with unrelated types are a type mismatch, not an inferred union; in particular no anonymous enum is synthesized (see [`enums-and-pattern-matching.md`](enums-and-pattern-matching.md)).

In condition positions, a following `{` begins the control-flow body rather than being reinterpreted as a struct literal attached to the condition expression. Use explicit syntax/parenthesization where needed to avoid aggregate-literal ambiguity.

## `match`

```omega
match value {
    0..=9 => "single digit",
    10..<100 => "double digit",
    .. => "large",
} else {
    "out of range"
}
```

`match` is an expression when its arms produce values. Pattern forms, exhaustiveness, ranges, overlap rules, and enum narrowing are specified in [`enums-and-pattern-matching.md`](enums-and-pattern-matching.md).

Arm results follow the same rules as `if` branches: a surrounding expected type is checked against every arm body, catch-all, and `else` block, and without one the arms must already agree on a type.

## Loops

Omega has four loop forms:

```omega
while condition { ... }
loop { ... }
for mut i := 0; i < 10; i += 1 { ... }
for value in source { ... }
```

### `while`

`while condition { body }` repeatedly evaluates a `bool` condition before each iteration.

### `loop`

`loop { body }` is unconditional. If the body contains no `break` targeting that loop, the loop is considered diverging and may satisfy a `never` return position. `while true` and `for ;;` are not given that same divergence guarantee; `loop` is the explicit spelling for an unconditional loop.

### Classic `for`

The classic form is:

```text
for [init] ; [condition] ; [post] { body }
```

The initializer and post clauses are optional. The parser accepts an omitted condition, but a valid Omega program currently requires the condition to be present and `bool`; `for ;; { ... }` is therefore rejected. There are no parentheses around the clauses. The initializer may use ordinary binding/statement forms.

### `for ... in`

`for [mut] name [: Type] in expression { body }` uses Omega's iteration/range rules. See [`iteration-and-ranges.md`](iteration-and-ranges.md).

## `break`, `continue`, and `defer`

`break` exits the innermost applicable loop. `continue` begins its next iteration.

`defer statement` schedules the statement for execution when the **enclosing function** exits, not when the current block ends. Defers execute in reverse order of registration (FILO) for that function invocation. The function's return value is determined before its deferred statements execute.

Current Omega rejects `defer` while lexically inside a loop and rejects a `defer` nested inside another deferred body. These are current language/implementation limitations tracked in [`../issues/language-limitations.md`](../issues/language-limitations.md).

Omega has no exception/`try`/`catch` mechanism in the current language.

## Boolean operators

`bool` supports eager `&`, `|`, `^`, logical negation `!`, and short-circuit `&&`/`||`:

```omega
a & b       # evaluate both
a | b       # evaluate both
a ^ b       # evaluate both
!a
a && b      # evaluate b only when a is true
a || b      # evaluate b only when a is false
```

The short-circuit behavior is equivalent to:

```text
!x      => x ^ true
x && y  => if x { y } else { false }
x || y  => if x { true } else { y }
```

Arithmetic, ordering, shifts, and unary `~` are not defined on `bool`; see [`types-and-primitives.md`](types-and-primitives.md).

## Operator precedence

From loosest to tightest:

```text
assignment (=, +=, -=, *=, /=, %=, &=, |=, ^=, <<=, >>=)
logical or (||)
logical and (&&)
comparison (== != < > <= >=)       # non-associative
bitwise or (|)
bitwise xor (^)
bitwise and (&)
shift (<< >>)
additive (+ -)
multiplicative (* / %)
unary (- ! * & &mut ~ ++ -- reveal comp)
cast (<Type>expr)
postfix (call, index/slice, field access)
```

Comparison is non-associative: `a == b == c` is invalid without explicitly grouping into boolean operations.

Bitwise operators bind more tightly than comparisons, so `a & b == c` means `(a & b) == c`. `&&` and `||` bind more loosely than comparisons, so `a >= x && a <= y` needs no extra grouping.

`&` is address-of in prefix position and bitwise-and in infix position. `*` and `-` similarly have prefix/infix meanings according to position. The token `&&` is always logical-and; write `& &p` when two consecutive address-of operations are intended.

## Binary-operand literal inference

For non-comparison arithmetic/bitwise expressions, an expected type for the whole expression may flow to adaptable operands. Otherwise the left operand is analyzed first and can provide an expected type for an adaptable right-hand literal.

```omega
mut i : u32 = 0;
i = i + 1;            # 1 adapts to u32
```

Comparison results are `bool`, so a surrounding expected `bool` does not become an expected numeric type for the operands. Explicitly typed operands never change merely because another operand or surrounding expression suggests a different type; incompatibility remains a type error.
