# `for` .. `in` loops

```
struct MyCustomStringIterator : Iterator<char> {
    exposed current: *u8;
    exposed last: *u8;

    exposed next(*mut self) => Option<char> {
        if self.current == self.last {
            return Option<char>::None;
        }
        c := <char>*self.current;
        self.current = <*u8>(self.current + 1);
        Option<char>::Some { value = c; }
    }
}

struct MyCustomString : ToIterator<char> {
    exposed ptr: *u8;
    exposed len: usize;

    exposed to_iterator(*self) => MyCustomStringIterator {
        MyCustomStringIterator { current = self.ptr; last = <*u8>(self.ptr + self.len); }
    }
}

for c in my_custom_str {
    printf(<*u8>b"%c-\0", <u8>c);
}
```

The iteration-protocol loop, distinct from the classic C-style `for init;
cond; post { }` (both start with `for`; the parser disambiguates by
lookahead — `for <mut>? ident in` commits to this grammar, everything else
falls through to the three-clause one unchanged). `for <mut>? binding in
iterator { }` — exactly one plain identifier binding, no destructuring
(this language has none, anywhere — see below for why that mattered more
than usual here).

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

`for x in y` only compiles when `y`'s type *nominally* declares `:
ToIterator<T>` **or** `: Iterator<T>` directly — checked directly against
the type's own declared `implements` clause
(`Analyzer::for_in_source_declares`), not merely "does a method named
`to_iterator`/`next` happen to resolve," which was this feature's one
significant gap in an earlier iteration. A type with a same-shaped
`to_iterator`/`next` pair but neither declaration is rejected with a
dedicated `ForLoopSourceNotIterable` diagnostic rather than silently
accepted or failing with a confusing, unrelated error.

```
struct Counter : Iterator<i32> {
    exposed value: i32;
    exposed limit: i32;
    exposed next(*mut self) => Option<i32> {
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

## Caveats

- **`*str`/`*[T]` don't implement `ToIterator` yet.** `for c in
  some_str { }` needs a hand-written wrapper struct today (as in the
  example above). Wiring the built-ins up is a natural follow-up using the
  exact same `for`-attachment mechanism `core::strings`/`core::slices`
  already use (see [specs](08-specs.md)) — not done as part of this
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
