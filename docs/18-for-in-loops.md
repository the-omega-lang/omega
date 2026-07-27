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

    exposed to_iterator(*self) => spec *mut Iterator<char> {
        it := malloc(<usize>sizeof<MyCustomStringIterator>);
        it.current = self.ptr;
        it.last = <*u8>(self.ptr + self.len);
        return it;
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
    to_iterator(*self) => spec *mut Iterator<T>;
}
```

Mirrors Rust's own `Iterator`/`IntoIterator` split: `Iterator<T>` is the
cursor, `ToIterator<T>` is what a collection implements to produce one.
`next` returns `core::option::Option<T>` (`None`/`Some { value: T }`) —
any type may implement `ToIterator<T>`/`Iterator<T>` and immediately work
with `for`, the same nominal way any other spec pair does.

`to_iterator`'s return is a **trait object** (`spec *mut Iterator<T>`),
not some implementor-specific concrete type — this spec system has no
associated-type mechanism to say "returns whatever type implements
`Iterator<T>`" (Rust's `IntoIterator::IntoIter` associated type), so a
dynamic-dispatch handle is the only way to express the contract at all
today. Two real consequences, both deliberate, not oversights:

- **Every `for`-loop pays one indirect call per element** (`next()`),
  not just once per loop — genuine, ongoing overhead, unlike Rust's fully
  static-dispatched iterators. True zero-cost iteration would need real
  associated-type machinery this compiler doesn't have; that's legitimate
  future work, not something worth blocking this feature on.
- **The returned iterator's storage must outlive the loop.** `to_iterator`
  only receives `*self` (immutable), so it can't stash the iterator inside
  a field of the source collection — the mutable state has to live
  somewhere else that survives the call returning. Heap-allocating it
  (`malloc`, as in the example above) is the correct, expected pattern —
  **not** returning `&mut` a stack local, which dangles the instant
  `to_iterator` returns (an ordinary use-after-return bug, no different
  from the same mistake in C; this compiler has no escape analysis to
  catch it).

## Desugaring

Analyzed, not parsed, into the equivalent hand-written form — `for` is
the *only* new grammar this feature added; everything else reuses
already-proven machinery (`Analyzer::analyze_for_in`,
`compiler/omega-analyzer/src/analysis/stmts.rs`):

```
{
    $iter := <iterator>.to_iterator();
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
  Concretely: `to_iterator` is called via **static** dispatch when the
  loop's source expression has a statically-known concrete type (the
  common case), and only `next()` — always called on the now-erased
  `spec *mut Iterator<T>` handle — pays the vtable indirection mentioned
  above.
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

## Disambiguating which spec instantiation to use

```
for n in <spec *ToIterator<u64>>&ambiguous_iterator {
    ...
}
```

A type may implement `ToIterator<T>` more than once, at different `T` (see
[specs](08-specs.md)'s "Implementing the same generic spec more than
once") — `for x in y` picks whichever one `y.to_iterator()` resolves to
through ordinary overload resolution, same as any other overloaded
method call. An explicit `<spec *Spec<Args>>expr` cast disambiguates when
that's not enough on its own, by forcing `y`'s type to the target
instantiation before the loop ever looks at it — this also works as a
general expression, independent of `for`-loops (see
[specs](08-specs.md)'s "Casting into a spec object" section).

In practice, this exact disambiguation scenario can't currently arise for
`ToIterator<T>` specifically: `to_iterator(*self)` takes no parameter that
varies with `T`, only the return type does, and this language has no
return-type-only overloading (confirmed intentional — see
[functions](00-functions.md)) — so two `to_iterator` overloads differing
only in `T` collide as an outright redeclaration before a `for`-loop is
ever involved. The cast support was still worth building on its own
merits (explicit casting into a `spec *T` never worked at all before this,
only 4 sites of *implicit* coercion did — see
[specs](08-specs.md)'s caveats), and it's there for the day some other
spec pair (or a reshaped `ToIterator<T>`) actually needs it.

## Caveats

- **`*str`/`*[T]` don't implement `ToIterator` yet.** `for c in
  some_str { }` needs a hand-written wrapper struct today (as in the
  example above). Wiring the built-ins up is a natural follow-up using the
  exact same `for`-attachment mechanism `core::strings`/`core::slices`
  already use (see [specs](08-specs.md)) — not done as part of this
  feature, to keep its own scope to the language mechanism and the two
  specs it depends on.
- **No zero-cost/fully-static iteration** — see "`core::iterator` /
  `core::option`" above; would need real associated-type support.
- **`Option<T>` itself has no convenience methods** (`is_some`,
  `unwrap_or`, ...) — see [core library](13-core-library.md).
