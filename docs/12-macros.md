# Macros

```
macro signed_integer($T: type) => items {
    spec SignedIntegerOps : Eq, Ord, Default for $T {
        equals(*self, other: Self) => bool { *self == other }
        ...
    }
}

signed_integer!(i8);
signed_integer!(i32);
```

```
macro make_point_type($name: type) => items {
    struct $name {
        exposed x: i32;
        exposed y: i32;
        exposed magnitude_sq(*self) => i32 { self.x * self.x + self.y * self.y }
    }
}
```

`macro name($a: expr, $b: type, ...) => expr | items { ... }` — a purely
syntactic, compile-time `SourceModule -> SourceModule` transform. Two
output kinds: `expr` (usable anywhere an ordinary expression can appear),
`items` (usable only at module top level, expanding to zero or more
top-level items — structs, specs, functions, ...). Two fragment kinds for
parameters: `expr` and `type`, deliberately small rather than open-ended —
adding a third (e.g. `ident`, `stmt`) is one new enum variant plus one new
match arm, not an architectural change.

## Mechanism

A macro's body is captured as a **raw token list** at definition time, not
parsed as `Expression`/`Statement`/`Item` right away — it legitimately
contains `$name` metavariables that aren't valid identifiers on their own,
and for an `items`-output macro, syntax that only becomes valid once
`$name` is substituted with a real identifier (`struct $name { ... }`).
Expansion substitutes each invocation's arguments into a copy of that token
stream and feeds it directly into the ordinary parser's normal entry
points — no render-to-text-and-relex round trip. Every individual token
keeps its own real originating span (from the definition's body, or from
the invocation's arguments), so diagnostics inside expanded code still
point somewhere real, even though a composite span built from a spliced
stream may not describe one single contiguous file range.

By the time macro expansion finishes, **no macro-related node survives
anywhere downstream** — HIR lowering has `unreachable!()` arms for
`MacroDefinition`/`MacroInvocation`, so nothing past `omega-parser` (HIR
lowering, analysis, codegen) needs any notion of macros existing at all.

## Duck-typed expansion

A macro's body is never type-checked or even syntax-checked on its own —
only once fully substituted with concrete arguments at a specific
invocation site, exactly like hand-written code. Whatever the substituted
code does or doesn't support is discovered the same way it would be for
anything else; there's no separate macro-hygiene or macro-specific
type-checking pass.

## Why no gensym/hygiene machinery exists

Unlike Rust's own mangling scheme, `omega-mangle`'s v0 grammar
deliberately has no disambiguator-index for macro expansion — expanded
items go through the exact same `Redeclaration`/overload-duplicate checks
hand-written declarations do, and once a symbol's full signature is part of
its mangled name (see [modules & linkage](10-modules-and-linkage.md)), two
genuinely distinct declarations can never collide, expanded or not. This is
possible specifically because macro expansion here has no closures and no
per-invocation hygiene scope to disambiguate in the first place.

## Where it's actually used

`runtime/core/core/numerics.omg` is the canonical real-world use: three
macros (`signed_integer`/`unsigned_integer`/`float_ops`), each invoked once
per concrete numeric type (twelve invocations total) rather than
hand-writing twelve near-identical `spec ... for $T` blocks — see
[core library](13-core-library.md).
