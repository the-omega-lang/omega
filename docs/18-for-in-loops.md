# `for` .. `in` loops

## Ranges

```
r := 1..<10;          # a value: Range<i32>, [1, 10)
for value in r { }    # iterated through ToIterator, like anything else
for value in 1..=10 { }
```

Expression-position `a..<b`, `a..=b`, `a..`, and `..<b` construct a tangible
`core::range::Range<T>` value — inert data with `start`, `end` and an
`inclusive` flag, freely stored, copied, passed and returned. `for` obtains a
separate `RangeIterator<T>` cursor from it through the ordinary
`ToIterator`/`Iterator` protocol, so there is **no range-specific machinery in
the compiler at all**: a range is iterable for exactly the reason a `List` is.
Because the range and the cursor are different types, iterating a range twice
counts twice — the value never carries iteration state.

`..` is the *contextual* range operator: it is the spelling for "nothing is
written on this side — work it out from where I am". `..<` (end excluded) and
`..=` (end included) are different tokens, and both always carry a written
end.

Which side is inferred is independent of which token you use:

```
a..<b   a..=b   # both bounds written
..<b    ..=b    # start inferred, end written
a..             # start written, end inferred
..              # both inferred
```

An end bound may never follow `..`, so `a..b` and `..5` are both rejected, for
the same reason: an end turned up after the token that means "no bound here".
Writing an end at all — with or without a start — takes `..<` or `..=`,
because a range naming its end has to say whether the end is in it.

An inferred side resolves according to where the range is written:

| Position | An inferred side means |
|---|---|
| expression | the element type's domain limit, via `core::range::Bounded` |
| index (`&items[5..]`) | the container's own length |
| match pattern | the arm's unmatched remainder |

Contextual inference always wins over domain inference: `&items[5..]` is "from
5 to the end of `items`", never "to `usize::MAX`". That is also why slicing a
`*[?]T` with an inferred end is an error — an unsized base carries no length
to infer from. One consequence worth stating plainly: a *stored* range cannot
express "the rest of a container", because "the rest" is a property of the
container, not of the range. `r := 5..; &items[r]` means `items[5..=usize::MAX]`,
which is out of bounds.

Bare `..` infers both sides at once, so on its own it has no type source and
is rejected (`a := ..;`). It is meaningful only where something supplies the
missing information — an index, a match arm, or an expected `Range<T>`.

### Which types can be ranged over

`Range<T>` is constructible for any `T`. Iterating one needs
`T: core::range::Successor` (`greater_than`, `equals`, `successor`), and an
inferred bound additionally needs `T: core::range::Bounded`. `core` conforms
every integer type to both, so user types are on exactly equal footing — a
custom index type that conforms is range-iterable with no compiler
involvement.

`successor` returns `Option<Self>`, yielding `None` at the type's maximum.
That is what lets `low..=255u8` terminate without ever computing `255 + 1`.

Floats are deliberately excluded: `f32`/`f64` conform to `Eq` but have no
total order (see [`core::cmp`](13-core-library.md)), so they get no
`Successor`. `char` does conform: its successor skips the UTF-16 surrogate
block (`U+D7FF` steps to `U+E000`), so character ranges use the same ordinary
protocol as integer ranges.

```
struct MyCustomStringIterator {
    exposed current: *u8;
    exposed last: *u8;
}

conform MyCustomStringIterator to Iterator<char> {
    next(*mut self) => Option<char> {
        if self.current == self.last {
            return Option<char>::None;
        }
        c := <char>*self.current;
        self.current = <*u8>(self.current + 1);
        Option<char>::Some { value = c; }
    }
}

struct MyCustomString {
    exposed ptr: *u8;
    exposed len: usize;
}

conform MyCustomString to ToIterator<char> {
    to_iterator(*self) => MyCustomStringIterator {
        MyCustomStringIterator { current = self.ptr; last = <*u8>(self.ptr + self.len); }
    }
}

for c in my_custom_str {
    printf(<*u8>b"%c-\0", <u8>c);
}
```

The iteration-protocol loop, distinct from the classic C-style `for init;
cond; post { }` (both start with `for`; the parser disambiguates by
lookahead — `for <mut>? ident in` (or `for <mut>? ident : Type in`) commits
to this grammar, everything else falls through to the three-clause one
unchanged). `for <mut>? binding [: Type] in iterator { }` — exactly one plain
identifier binding, no destructuring
(this language has none, anywhere — see below for why that mattered more
than usual here).

A **range-driven** `for i in a..<b { }` shares this same `for <mut>?
binding in <expr>` grammar but desugars completely differently — see
"Range-driven `for`" below, after the iteration-protocol's own desugaring.

## `core::iterator` / `core::option`

```
spec Iterator<T> {
    next(*mut self) => Option<T>;
}
spec ToIterator<T> {
    to_iterator(*self) => spec Iterator<T>;
}
```

Mirrors Rust's own `Iterator`/`IntoIterator` split: `Iterator<T>` is the
cursor, `ToIterator<T>` is what a collection implements to produce one.
`next` returns `core::option::Option<T>` (`None`/`Some { value: T }`) —
any type may implement `ToIterator<T>`/`Iterator<T>` and immediately work
with `for`, the same nominal way any other spec pair does.

`to_iterator`'s return, `spec Iterator<T>` (no `*`), is **static
dispatch** — see [specs](08-specs.md)'s own section on the two spec-object
forms. Each implementor returns its own concrete iterator type *by value*
(Rust's `IntoIterator::IntoIter` equivalent, checked against the
`Iterator<T>` bound rather than matched by exact signature), not a
dynamic-dispatch fat pointer — so:

- **No heap allocation, no indirect call.** The iterator is an ordinary,
  fully monomorphized value; `next()` resolves through the same static
  method-call machinery any concrete-type method call already does. There
  is no `spec *mut Iterator<T>` handle anywhere in this feature any more,
  and correspondingly no per-element vtable indirection — this is now
  genuinely zero-cost iteration, matching Rust's.
- **`ToIterator<T>` (and any other spec using a `spec T` return
  requirement) is not object-safe** — `spec *ToIterator<T>` doesn't exist
  (no vtable slot can represent "whichever concrete type each implementor
  happens to use"). A deliberate, accepted tradeoff for the associated-
  type-like expressiveness this buys — see [specs](08-specs.md)'s
  object-safety caveat.

## Real, nominal conformance

`for x in y` only compiles when `y`'s type nominally conforms to
`ToIterator<T>` **or** `Iterator<T>` — checked against the conform registry,
not merely "does a method named
`to_iterator`/`next` happen to resolve," which was this feature's one
significant gap in an earlier iteration. A type with a same-shaped
`to_iterator`/`next` pair but neither declaration is rejected with a
dedicated `ForLoopSourceNotIterable` diagnostic rather than silently
accepted or failing with a confusing, unrelated error.

```
struct Counter {
    exposed value: i32;
    exposed limit: i32;
}
conform Counter to Iterator<i32> {
    next(*mut self) => Option<i32> {
        if self.value >= self.limit { return Option<i32>::None; }
        v := self.value;
        self.value += 1;
        Option<i32>::Some { value = v; }
    }
}

for x in Counter { value = 0; limit = 5; } {
    printf(<*u8>b"%d \0", x);
}
```

**An iterator/cursor is directly usable in `for`, with no `ToIterator`
wrapper needed** — mirroring Rust's blanket `impl<I: Iterator> IntoIterator
for I`. `Analyzer::classify_for_in_source` tries `ToIterator<T>` first (an
explicit `ToIterator` impl always wins over treating the source as its own
iterator, matching Rust's explicit-impl-beats-blanket-impl precedence);
only if that's absent does it check `Iterator<T>` directly, in which case
`f.iterator`'s own already-checked value becomes `$iter` verbatim — no
`.to_iterator()` call is synthesized at all in that case.

If a source conforms to `ToIterator<T>` more than once, the element type is
ambiguous. Select one explicitly: `for value : u8 in source { ... }`. The
annotation is matched against the conformed `ToIterator<u8>` argument rather
than treated as a post-hoc cast of the loop binding.

## Desugaring

Analyzed, not parsed, into the equivalent hand-written form — `for` is
the *only* new grammar this feature added; everything else reuses
already-proven machinery (`Analyzer::analyze_for_in`,
`compiler/omega-analyzer/src/analysis/stmts.rs`):

```
{
    mut $iter := <iterator>.to_iterator();  # or just `<iterator>` -- see below
    while true {
        $next := $iter.next();
        match $next {
            Option::None => { break; }
            Option::Some => {
                <mut>? binding := $next.value;
                <body, spliced in unchanged>
            }
        }
    }
}
```

`$iter` is declared `mut` — `next(*mut self)` needs a mutable pointer to
call through, and only a binding actually declared `mut` can ever have one
taken to it (see [variables & mutability](02-variables-and-mutability.md)).
When the source declares `Iterator<T>` directly rather than `ToIterator<T>`
("Real, nominal conformance" above), `$iter`'s own initializer is
`<iterator>` itself, not a `.to_iterator()` call on it.

This is why the feature needed **zero new MIR/codegen surface**: the
result is built entirely out of `CheckedStmt::While`/`CheckedExpr::Match`/
ordinary `FunctionCall`/`DynamicCall` nodes, all already fully supported.
`break`/`continue` written in the loop body work exactly as expected —
they resolve against the synthesized `while true`, the same loop-stack
discipline every other loop already uses, so nesting composes for free.

Two things worth knowing about *how* it's built, not just *that* it
works:

- **`to_iterator`/`next` are resolved as ordinary method calls** — ordinary
  overload resolution, auto-ref, and static-vs-dynamic-dispatch selection
  all apply exactly as they would if you wrote `x.to_iterator()` by hand.
  Both calls are ordinary static dispatch in the common case now that
  `to_iterator` returns a concrete value rather than a `spec *T` handle —
  dynamic dispatch only enters the picture if `Iterator<T>`'s own concrete
  implementor is itself coerced into some *other* `spec *Something`
  pointer independently, unrelated to this desugaring.
- **The `match` is hand-built, not synthesized as source text.** This
  language's `match` has no destructuring pattern syntax at all —
  `Option::Some` doesn't bind a name on its own; only *narrowing* an
  already-named scrutinee does (see [enums & pattern
  matching](05-enums-and-pattern-matching.md)). Since `$next` is a
  synthetic local with no source-level name a pattern could reference,
  the match is built directly against `$next`'s already-known variant
  layout (`core::option::Option`'s variant order — `None` = 0, `Some` = 1
  — is load-bearing here) using the same narrowing primitives `match`'s
  own analysis uses internally, not by generating and re-parsing text.

## Range-driven `for`

```
for i in 0..<10 { ... }     # exclusive end
for i in 0..=10 { ... }     # inclusive end
for i in 10.. { ... }       # open end -- counts up to the element type's own max
```

`for i in <range> { }` recognizes a *literal* range expression in the
iterator position (`Expression::Range`/`HirExpr::Range` — see
[enums & pattern matching](05-enums-and-pattern-matching.md)'s "Ranges")
and intercepts it in `Analyzer::analyze_for`, *before* `classify_for_in_source`
ever runs — there is no first-class `Range` value anywhere in this
language (a range is a purely structural, compile-time concept everywhere
else too, consumed directly the same way a slice's own `base[range]`
already is), so this never goes through `ToIterator`/`Iterator` at all.
Instead it desugars directly into the classic three-clause `for`'s own
`CheckedFor` shape (`Analyzer::analyze_for`) — reusing that machinery
unchanged, not a third independent code path in MIR/codegen.

A start is mandatory (`for i in ..b { }`/bare `for i in .. { }` are both
`ForLoopRangeMissingStart` — unlike a slice's own missing start, there's
no principled value to begin counting from) and decides the loop
variable's own type, which must be a real, steppable integer kind
(`i8`..`i64`/`isize`, `u8`..`u64`/`usize` — `ForLoopRangeElementNotSupported`
otherwise). `char`/`bool` are deliberately excluded from this legacy
counter-based loop even though both have an `integer_domain()` and are legal
`match`-range bounds: `bool` has no arithmetic at all, and `char` deliberately
requires `Successor`-based `for .. in` iteration rather than arithmetic
stepping. An explicit end's type must match the start's exactly
(`ForLoopRangeBoundTypeMismatch` otherwise — no implicit conversions here
either); an open end (`a..`) implicitly uses the element type's own
`integer_domain().1` (its real maximum).

A private counter (`$i`, never user-visible) drives the loop; the `binding`
you actually write is a fresh copy taken from it each iteration (`<mut>?
binding := $i;`), exactly like the iteration-protocol desugaring's own
`binding := $next.value` above — decoupled from the counter's own
mutability needs, which this desugaring alone controls. Reassigning
`binding` inside the loop body (if declared `mut`) never affects the
loop's own iteration.

**Overflow safety, not left as an afterthought**: an exclusive end
(`a..<b`) desugars to the obvious `while $i < b { ...; $i += 1; }` shape,
which is provably safe (`b` is itself always representable, so the last
value `$i` is ever incremented from is `b - 1`). An inclusive end (`a..=b`,
and `a..`'s implicit `b = domain.max`) can't safely use that shape — if
`b` happens to be the element type's own actual maximum, incrementing past
the last iteration would overflow. That case uses a `$more`-flag shape
instead, which never computes `b + 1` at all: `$more` is flipped to
`false` (rather than `$i` incremented) the moment `$i` reaches `b`. Both
shapes are ordinary, fully-inlined `CheckedFor` init/condition/post
clauses — no hidden allocation, no runtime overflow check needed because
none is ever reachable.

## Caveats

- **`*str`/`*[]T` don't implement `ToIterator` yet.** `for c in
  some_str { }` needs a hand-written wrapper struct today (as in the
  example above). Wiring the built-ins up is a natural follow-up using the
  the same generic conform mechanism collections use (see
  [specs](08-specs.md)) — not done as part of this
  feature, to keep its own scope to the language mechanism and the two
  specs it depends on.
- **`Option<T>` itself has no convenience methods** (`is_some`,
  `unwrap_or`, ...) — see [core library](13-core-library.md).
- **A type implementing `ToIterator<T>` more than once, at different `T`,
  has no way to disambiguate which `for x in y` picks** — unlike an
  ordinary overloaded method call, there's no argument shape to resolve
  against (`to_iterator(*self)` takes none), and the explicit-cast
  disambiguation the dynamic-dispatch design used to offer
  (`<spec *ToIterator<u64>>expr`) no longer applies now that `ToIterator<T>`
  isn't object-safe. Narrow in practice (this scenario needs two
  `to_iterator` overloads differing only in return type, which most specs
  won't hit), but a genuine, currently-unsolved gap if it comes up.
