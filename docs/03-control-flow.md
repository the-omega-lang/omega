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
    0..=9 => "single digit",
    10..<100 => "double digit",
    .. => "large",
} else {
    "out of range"
}
```

See [enums & pattern matching](05-enums-and-pattern-matching.md) for the
full pattern grammar (including the `..` catch-all arm above), exhaustiveness
checking, and variant narrowing.

## Loops

```
while condition { ... }

loop { ... }                    # unconditional -- see below

for mut i := 0; i < 10; i += 1 { ... }
for ; condition; { ... }        # while-equivalent
for ;; { ... }                  # infinite

for i in 0..<10 { ... }         # range-driven -- see below
```

`for` is C-style — three semicolon-separated clauses, **each independently
optional**, but **no enclosing parens**. `for`'s `init` clause reuses the
same declaration/walrus statement shapes ordinary statements have; the
`post` clause sits directly before the body's `{` with no separating `;`
— an *empty* post clause is disambiguated from "the post clause is empty
and this `{` starts the body" by peeking for `{` first, never attempting
to parse an expression there.

**A range-driven `for i in a..<b { }`/`a..=b`/`a.. { }`** desugars
directly into this same three-clause shape (never a real `Range` value —
see [`for`..`in` loops](18-for-in-loops.md)) — `a` decides the loop
variable's own type (a real integer type only; `char`/`bool` aren't
supported, see [enums & pattern matching](05-enums-and-pattern-matching.md)'s
"Ranges" section), and an omitted start (`for i in ..b { }`/bare `for i
in .. { }`) is rejected outright rather than defaulting to anything —
unlike a slice's own missing start, there's no principled value to begin
counting from.

**`loop { ... }` is the one loop form the compiler can *prove* always
repeats** (unless a `break` targeting it is found anywhere in its own
body) — unlike `while true { ... }`/`for ;; { ... }`, which are
*conditional* loops that merely happen to never see their condition turn
false. That distinction is what makes `loop` able to satisfy a `never`
return type (see [primitives](01-primitives.md)'s "`never`: not a
conventional type") and produce real unreachable-code warnings for
whatever follows it with no way out; `while`/`for` deliberately never get
this treatment, no matter how the condition is spelled, so a `while true
{ }`/`while <a compile-time-constant true> { }` gets a `PreferLoop`
warning suggesting `loop` instead of trying to make `while` smart about
recognizing it too. The check is purely syntactic (does a `break`
targeting *this* loop appear anywhere in its body?), not real reachability
analysis — `loop { if cond { break; } }` is correctly *not* treated as
provably diverging, even though it happens to loop forever whenever
`cond` is false.

`break`/`continue` exist as ordinary statements. `defer <statement>;` /
`defer { ... }` schedules a statement to run when the *enclosing function*
exits (not the enclosing block) — Omega's only structured cleanup
mechanism; there is no `try`/`catch`/exceptions in the language at all.

A third loop form, `for <mut>? binding in iterator { ... }`, iterates
anything implementing `core::iterator::ToIterator<T>` — see
[for-in loops](18-for-in-loops.md) for the full story (it's involved
enough to warrant its own chapter: a real spec-backed protocol, not just
grammar).

## Boolean operators — native `bool`, but still no `!`

`bool` supports `== != & | ^` directly, staying `bool` (no `numeric_kind`
of its own is needed for these — see [primitives](01-primitives.md)'s
"`char`, `bool`, and pointer arithmetic" section for the full story,
including why `char` and pointers work differently, coercing to a numeric
type instead of staying native). This is sound specifically because
`bool` is *closed* under all five: combining two valid `bool`s (`0`/`1`)
any of those ways is still a valid `bool`.

There is still **no `!` prefix operator in the grammar at all** (`!` only
ever appears as part of `!=`), and
still no arithmetic/shifts on `bool` (`true + true` has no meaning to fall
back on) — see [primitives](01-primitives.md)'s caveats for why `!`
specifically is a bigger addition than it looks (a new grammar token, not
just an analyzer change) and is left as deliberate future work. Until then,
negation is nested `if`-expressions:

```
if x { false } else { true }          # NOT x
if x { y } else { false }              # x AND y (short-circuits)
if x { true } else { y }                # x OR y (short-circuits)
```

`std::cmp`'s `Eq`/`Ord` default methods and every predicate in
`core` are written this way. Comparison is also **non-associative**
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
unary (- * & &mut ~ reveal)
cast (<Type>expr)
postfix (call, index/slice, field access)
```

Bitwise precedence deliberately follows *Rust's*, not C's — `a & b == c`
parses as `(a & b) == c`, avoiding C's classic footgun. `&` stays
dual-purpose (prefix = address-of, infix = bitwise-and), disambiguated
purely by parser position, the same precedent `*`/`-` already set as both
prefix and infix operators.

## Untyped-literal inference in binary-op operands

```
mut i: u32 = 0;
i = i + 1;                    # `1` adapts to `u32`, not `i32`

x: i64 = -7;
abs := if x < 0 { -x } else { x };    # `0` adapts to `i64`
```

A binary operator's two operands are inferred with two composed rules,
neither of them new — both mirror precedent this language already
committed to elsewhere:

1. **The outer `expected` type this whole expression itself received
   flows to both operands, but only for a non-comparison op.** An
   arithmetic/bitwise result *is* its operand type, so an outer numeric
   `expected` (e.g. `i = i + 1`'s assignment target, `u32`, already
   resolved before the value is analyzed) legitimately flows through. A
   comparison's result is always `bool` regardless of its (numeric)
   operands, so this deliberately does **not** apply there — threading a
   `bool` expectation into two numeric operands would be nonsensical.
2. **Left is always analyzed first; absent an outer `expected`, its own
   resolved type becomes `expected` for the right operand** — the exact
   same "earliest position is the anchor" rule `if`-expression branches
   already use (see below), not a new inference philosophy. For a
   non-comparison op, the anchor is left's type *after* the `char`/pointer
   coercion described in [primitives](01-primitives.md), not before —
   `some_char + 1` needs the bare `1` to adapt to `u32` (what `some_char`
   is about to become), not to `char` (which isn't numeric, so it would
   just fall back to `1`'s own default of `i32` and then mismatch).

Both rules are safe unconditionally: `expected` is never a coercion
mechanism anywhere in this language, only a hint consulted by genuinely
adaptable things (a bare literal, mainly) — an already-concretely-typed
operand ignores it outright, and the ordinary exact-type-equality check
between operands still runs afterward, unchanged. So this can only turn a
previously-failing narrowing case into a working one; it can never accept
a genuine mismatch it wouldn't have accepted before.

## Caveats

- `char` has comparison (`== != < <= > >=`) and can be used as a `match`
  scrutinee, including ranges (`'A'..='Z'`) — see
  [primitives](01-primitives.md) and
  [enums & pattern matching](05-enums-and-pattern-matching.md). Arithmetic/
  bitwise ops and casts are supported too, but by coercing to `u32` first
  (never back to `char` implicitly) — see [primitives](01-primitives.md)'s
  "`char`, `bool`, and pointer arithmetic" section.
- Binary-op literal narrowing is **earliest-wins, not most-specific-wins**
  — matching the identical, already-accepted trade-off `if`-expression
  branches make (`if true { 8 } else { 7u16 }` doesn't retroactively
  narrow branch 0 either). `0 < some_i64_var` (the literal written first,
  the concretely-typed operand second) still won't narrow — write
  `some_i64_var > 0` or cast explicitly instead. Not a gap left over from
  the fix above; a deliberate scope match with existing precedent.
- **Fixed: a bare, block-shaped `if`/`{ }`/`match` statement immediately
  followed by a new statement starting with `*`/`-`/`&` used to be
  misparsed** — the following line's leading operator was read as a
  binary operator continuing the block's own value into the next
  statement (e.g. `if cond { ... } \n *ptr = value;` parsed as `(if cond
  {...}) * ptr = value`), producing a confusing "invalid assignment
  target" error instead of two separate statements. Found repeatedly
  while writing `std`'s own collections (`List<T>::push`/`get`/`set`, all
  of which follow a capacity-check `if` with a pointer-deref statement).
  Root cause: a statement's leading expression parsed through the same
  shared precedence-climbing tiers as any other expression, so by the
  time anything checked whether the result was block-shaped, a following
  `*`/`-`/`&` (all three also valid infix continuations, at the
  multiplicative/additive/bitand tiers respectively) had already been
  folded in. Fixed by giving statement-leading position (and
  `parse_codeblock`'s own speculative tail-value attempt, which parses at
  the identical leading position) a dedicated entry point
  (`parse_statement_leading_expression`,
  `compiler/omega-parser/src/parser/expression.rs`) that, when the very
  next token starts `{`/`if`/`match`, parses only that block and returns
  immediately, skipping the climbing tiers entirely — matching Rust's own
  rule that a block-like expression in statement position is never
  continued as an operand by whatever follows it. Explicit continuation
  still works if genuinely wanted: wrap the block in parens
  (`(if cond {...}) * ptr`), which recurses into the ordinary expression
  grammar unaffected by this. See
  [the standard library](23-standard-library.md).
