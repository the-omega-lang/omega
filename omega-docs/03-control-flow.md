# Control flow

## `if` is an expression

```
result := if x > 0 { "positive" } else if x < 0 { "negative" } else { "zero" };
```

Every branch (including the anchor, branch 0) is analyzed in source order;
**branch 0 is always the literal-inference anchor** — every later branch or
`else` is checked *against* branch 0's own type, never the reverse. This was
deliberately changed from an earlier "peek every branch for the first
non-adaptable-literal one" design: `if true { 10 } else { 20u64 }` is a
type mismatch (branch 0's `10` defaults to `i32`) even though a later
branch could theoretically have "fixed" it — this earliest-wins rule is
simpler to reason about and matches how a reader encounters the branches.

A condition is parsed with **struct literals restricted** — `if flag { ...
}` always means "condition `flag`, then the body," never a `flag { ... }`
struct literal. This applies uniformly to `if`/`while`/`for`'s
condition-bearing clauses and `match`'s scrutinee.

## `match`

```
match value {
    Entity::Person { name, .. } => { ... },
    0...9 => "single digit",
    10..<100 => "double digit",
} else {
    "out of range"
}
```

See [enums & pattern matching](05-enums-and-pattern-matching.md) for the
full pattern grammar, exhaustiveness checking, and variant narrowing.

## Loops

```
while condition { ... }

for mut i := 0; i < 10; i += 1 { ... }
for ; condition; { ... }        # while-equivalent
for ;; { ... }                  # infinite
```

`for` is C-style — three semicolon-separated clauses, **each independently
optional**, but **no enclosing parens**. There is no dedicated `loop`
keyword; an infinite loop is `for ;; { ... }` or `while true { ... }`.
`for`'s `init` clause reuses the same declaration/walrus statement shapes
ordinary statements have; the `post` clause sits directly before the body's
`{` with no separating `;` — an *empty* post clause is disambiguated from
"the post clause is empty and this `{` starts the body" by peeking for `{`
first, never attempting to parse an expression there.

`break`/`continue` exist as ordinary statements. `defer <statement>;` /
`defer { ... }` schedules a statement to run when the *enclosing function*
exits (not the enclosing block) — Omega's only structured cleanup
mechanism; there is no `try`/`catch`/exceptions in the language at all.

## No boolean operators — this is deliberate, not missing

`bool` supports **none** of `== != & | ^`, and there is **no `!` prefix
operator in the grammar at all** (`!` only ever appears as part of `!=` or
macro-invocation syntax `name!(...)`). `bool` is excluded from
`numeric_kind` entirely, the same exclusion `char` gets — comparison and
bitwise operators only ever accept numeric operands in this compiler.

The only way to negate or combine booleans is nested `if`-expressions:

```
if x { false } else { true }          # NOT x
if x { y } else { false }              # x AND y (short-circuits)
if x { true } else { y }                # x OR y (short-circuits)
```

`core::cmp`'s `Eq`/`Ord` default methods and every predicate in
`omega-core` are written this way. Comparison is also **non-associative**
(`a == b == c` doesn't parse as chained comparison) and **binds looser than
the bitwise operators** (Rust-style, not C-style) — so combining two
comparisons with `&`/`|` needs full parenthesization: `(a >= x) & (a <= y)`,
never `a >= x & a <= y`.

## Operator precedence (loosest to tightest)

```
assignment (=, += , ...)
comparison (== != < > <= >=)      -- non-associative
bitor (|)
bitxor (^)
bitand (&)
shift (<< >>)
additive (+ -)
multiplicative (* / %)
unary (- * & &mut ~ hidden)
cast (<Type>expr)
postfix (call, index/slice, field access)
```

Bitwise precedence deliberately follows *Rust's*, not C's — `a & b == c`
parses as `(a & b) == c`, avoiding C's classic footgun. `&` stays
dual-purpose (prefix = address-of, infix = bitwise-and), disambiguated
purely by parser position, the same precedent `*`/`-` already set as both
prefix and infix operators.

## Caveats

- `char` has no comparison, no bitwise, and no cast support at all
  currently (see [generics & known gaps](06-generics.md)) — it can be
  constructed and passed through (e.g. as a `printf` vararg) but not
  compared or matched at the language level.
- Untyped integer literals don't reliably narrow across widths outside of
  a function's own tail-return position — see
  [primitives](01-primitives.md) and [generics](06-generics.md) for the
  full story; loop counters and comparison operands against a non-`i32`
  variable typically need an explicit cast or suffix (`<Self>0`, `1u32`).
