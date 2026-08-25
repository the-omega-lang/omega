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

## The try operator `?`

`expression?` propagates a typed failure out of the enclosing function. It is a
postfix suffix at the tightest precedence tier, so it chains left to right with
calls, indexing/slicing, and field access: `find(key)?.name` applies the field
access to the unwrapped value, and `nested()??` unwraps two layers.

The operator is defined on exactly two types, recognized by declaration
identity rather than by spelling: `core::option::Option<T>` and
`core::result::Result<T, E>`. A transparent alias of either resolves to the same
declaration and keeps the behavior; a separately declared type with the same
shape and variant names does not gain it. There is no user-extensible protocol
behind `?`.

- `Option<T>?` requires the enclosing function to return `Option<R>`. `Some`
  yields the `T` payload; `None` returns that enclosing function's own `None`.
- `Result<T, E>?` requires the enclosing function to return `Result<R, F>`.
  `Ok` yields the `T` payload; `Err` extracts the `E` payload, converts it to
  `F` under exactly the rules that apply where an `F` is explicitly expected,
  and returns the enclosing `Result<R, F>::Err`.

The two families never convert into one another, in either direction, and the
error conversion applies to the extracted payload only — a `Result<T, E>` value
still does not convert to a `Result<T, enum E | F>` as a whole. In practice the
payload rule is what lets a function returning `Result<R, enum E | F>` apply `?`
to both a `Result<_, E>` and a `Result<_, F>`, using the anonymous-enum
conversion described in
[`enums-and-pattern-matching.md`](enums-and-pattern-matching.md).

```omega
combined(key: i32, code: i32) => Result<i32, enum NotFound | Denied> {
	found := lookup(key)?;
	allowed := authorize(code)?;
	Result<i32, enum NotFound | Denied>::Ok { value = found + allowed; }
}
```

The success type is independent of the enclosing function's success type: `?`
produces `T`, and the surrounding expression applies its ordinary expected-type
rules to that value. The operand is evaluated exactly once. A failing `?` exits
the function exactly as an explicit `return` does, so any already-registered
`defer` runs. For the same reason `?` is rejected inside a `defer` body, matching
the existing prohibition on `return` there.

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
postfix (call, index/slice, field access, try `?`)
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
