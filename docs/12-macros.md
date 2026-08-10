# Macros

Macros are Omega's purely syntactic, compile-time `SourceModule ->
SourceModule` transform. They define a token template and use the ordinary
grammar at each invocation site to determine what that template must mean.

```
macro signed_integer($T: type) => {
    spec SignedIntegerOps : Eq, Ord, Default for $T {
        equals(*self, other: Self) => bool { *self == other }
        ...
    }
}

signed_integer$(i8);
signed_integer$(i32);
```

There is no declared output kind. The invocation position determines the
grammar used for the expanded tokens:

- In item position, `name$(...)` splices zero or more top-level items.
- As a complete statement, `name$(...);` splices zero or more statements.
- In expression position, `name$(...)` must parse as one expression.

An expansion that does not parse in its position reports, for example,
`macro 'm' does not expand to a valid expression here`. A statement macro
cannot be the direct body of `defer`, because it could become more than one
statement; wrap it in a block: `defer { name$(...); }`.

## Parameters and repetition

Parameters are `expr`, `type`, or `ident` fragments. `ident` accepts one
bare identifier; it is useful for a called function or generated name.

One trailing parameter may be variadic. It captures zero or more arguments:

```
macro call_each($f: ident, $args: expr...) => {
    $...(){ $f($args); }
}

macro call_with_args($f: ident, $args: expr...) => {
    $f($...(,){ $args })
}

main() => i32 {
    # both are statement position: `call_each` splices two statements,
    # `call_with_args` splices one (its expansion's tail expression).
    call_each$(puts, first, second);
    call_with_args$(printf, format, 1, 2);
    return 0;
}
```

`$...(){ body }` expands `body` once for each variadic argument. A separator
inside its parentheses is emitted only between adjacent expansions, so
`$...(,){ $args }` builds a comma-separated argument list and an empty
variadic argument list emits nothing. Repetitions cannot nest, and the
variadic metavariable must appear inside the repetition that expands it.

The `$` sigil has three distinct fixed forms:

```
$name                 metavariable
$...( separator? ) { body }  repetition
name$( arguments )    invocation
```

## Mechanism

A macro body is captured as a token tree at definition time, not parsed as
an `Expression`/`Statement`/`Item` right away — it legitimately contains
`$name` metavariables and syntax that becomes valid only after substitution.
Expansion substitutes tokens directly into the ordinary parser's relevant
entry point; there is no render-to-text-and-relex round trip. Every token
therefore keeps its real originating span from either the definition or the
invocation arguments.

By the time macro expansion finishes, no macro-related node survives
downstream: HIR lowering has `unreachable!()` arms for macro definitions and
invocations, so nothing past `omega-parser` needs to know macros exist.

## Duck-typed expansion

A macro body is never type-checked or syntax-checked on its own. It is
checked only after substitution at a concrete invocation site, exactly like
hand-written code. There is no macro-specific type checking pass.

## Why no gensym/hygiene machinery exists

Unlike Rust's own mangling scheme, `omega-mangle`'s v0 grammar deliberately
has no disambiguator-index for macro expansion. Expanded items go through
the same redeclaration and overload checks as hand-written declarations, and
once a symbol's full signature is part of its mangled name, genuinely
distinct declarations cannot collide. This is possible because macro
expansion has no closures and no per-invocation hygiene scope to disambiguate.

That reasoning covers *declarations*. It does not cover **locals a
statement-position expansion introduces**, which are spliced into the caller's
own block and therefore participate in ordinary lexical scoping: an argument
expression that names a caller variable will bind to a macro-introduced local
of the same name instead. Wrapping a macro body in `{ }` stops the local from
*leaking* outward, but it does not stop it from *capturing* inward — the block
is exactly the scope the argument is substituted into.

There is no mechanism that prevents this, so a macro that must introduce a
local picks a name a caller is unlikely to write. `core::io`'s print macros use
an `omega_print_` prefix for precisely this reason; a plainer `out` shadowed
any caller-supplied `out` passed as an argument, which is a real bug that a
real program hit (`examples/dev/main.omg` prints a `*str` named `out`).

## Where it's actually used

`runtime/core/core/numerics.omg` uses three macros
(`signed_integer`/`unsigned_integer`/`float_ops`) to generate numeric spec
implementations for every primitive type instead of hand-writing twelve
near-identical blocks. See [core library](13-core-library.md).

## Cross-file visibility

`macro` accepts the same hidden/default, `internal`, and `exposed`
visibility modifiers as ordinary items. Invocation resolution is local
definitions first, explicitly imported macros second, then exposed `core`
macros as an ambient fallback. Visibility is not transitive: an imported
module's imports are not re-exported. A nested invocation emitted by a macro
resolves at the call site; generated imports also arrive too late to add
macros to that expansion environment.
