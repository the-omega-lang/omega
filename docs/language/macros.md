# Macros

This chapter is normative for current Omega language behavior. Known implementation limitations are tracked separately under [`../issues/`](../issues/).

Macros are Omega's purely syntactic, compile-time `SourceModule ->
SourceModule` transform. They define a token template and use the ordinary
grammar at each invocation site to determine what that template must mean.

```
macro signed_integer($T: type) => {
    primitive $T {
        exposed equals(*self, other: Self) => bool { *self == other }
        ...
    }
    conform $T to Eq {}
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

Macro parameter names are unique within a signature. A fixed parameter and the trailing variadic parameter may not use the same name.

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

## Expansion model

A macro body is a token template rather than an independently valid Omega expression/item. Metavariables and repetition syntax are substituted at a concrete invocation, and the resulting token sequence must then satisfy the grammar required by that invocation position.

Tokens originating in the macro definition retain definition-site origin; tokens substituted from invocation arguments retain caller origin. This origin distinction is part of macro name-resolution/hygiene behavior described below.

Macro expansion is complete before ordinary semantic checking of the expanded declarations/statements/expressions.

## Duck-typed expansion

A macro body is never type-checked or syntax-checked on its own. It is
checked only after substitution at a concrete invocation site, exactly like
hand-written code. There is no macro-specific type checking pass.

## Definition-site hygiene

Macro-generated tokens follow a narrow origin-based hygiene model:

- names and paths written in the macro body resolve from the macro definition's module/lexical environment;
- tokens substituted from a macro argument retain the caller's origin and resolve in the caller's environment;
- a nested macro invocation written in the macro body resolves in the definition environment, while a nested invocation supplied as an argument resolves from the caller.

Generated declarations remain ordinary declarations and therefore participate in normal redeclaration/overload rules. Generic parameters and `Self` are substitution-bound rather than ordinary captured lexical names.

An `import` inside a macro body is invalid. Definition-origin paths already resolve in the definition module, and expansion is not allowed to mutate the caller's import namespace.

## Where it's actually used

`runtime/core/primitives/numerics.omg` uses three macros
(`signed_integer`/`unsigned_integer`/`float_ops`) to generate numeric spec
method and conformance declarations for every primitive type instead of
hand-writing twelve near-identical groups. See [core library](../guide/core-library.md).

## Cross-file visibility

`macro` accepts the same hidden/default, `shared`, and `exposed`
visibility modifiers as ordinary items. Invocation resolution is local
definitions first, explicitly imported macros second, then exposed `core`
macros as an ambient fallback. Visibility is not transitive: an imported
module's imports are not re-exported. A nested invocation emitted by a macro
resolves in that macro's definition environment. Macro bodies cannot contain
`import`: their own paths already use the definition module, while mutating
the caller's import namespace would be incoherent.
