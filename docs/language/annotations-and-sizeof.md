# Annotations and `sizeof`

Annotations are explicit compiler directives written immediately before supported declarations.

```omega
@layout(pack = sizeof<usize>, align = sizeof<usize>)
struct Header { ... }

@inline
@suppress(inline_not_enforced)
fast() => void { ... }

@mangling(disabled)
raw_add(a: i32, b: i32) => i32 { a + b }
```

## Grammar

```ebnf
annotation = "@", identifier, [ "(", [ arg, { ",", arg } ], ")" ] ;
arg        = identifier | identifier, "=", value ;
value      = decimal-integer | "sizeof", "<", type, ">" | string-literal ;
```

Bare `@name` and `@name()` both carry zero arguments.

Recognized annotations are `layout`, `inline`, `mangling`, `naked`, and `suppress`. Duplicate use of the same annotation on one declaration is an error. Unknown annotation names are errors.

## Applicability

| Annotation | Allowed declarations |
|---|---|
| `@layout` | `struct`, `enum` |
| `@inline` | functions/methods |
| `@mangling` | functions/methods, `foreign` bindings/functions, subject to restrictions below |
| `@naked` | functions/methods, subject to restrictions below |
| `@suppress` | `struct`, `enum`, `union`, function/method, `import`, `spec` |

Other item kinds do not acquire an annotation meaning merely because the generic `@...` syntax exists.

## `@layout(pack = n, align = n)`

Struct and enum layout defaults to:

```text
pack = 1
align = 1
```

Either key may be supplied independently and in either order. Each supplied value must be a non-zero power of two fitting `u32`.

- `align` controls the whole type's outward/embedding alignment and trailing size rounding.
- `pack` controls field-grouping granularity: fields share a `pack`-sized chunk when they fit; otherwise placement advances to the next chunk boundary. A field larger than `pack` may begin at a chunk boundary rather than being rejected.

A bare `@layout`/`@layout()` is equivalent to the defaults. Unions do not currently support `@layout`.

An annotation layout value may be a decimal integer or `sizeof<Primitive>`; the `sizeof` form in `@layout` is deliberately restricted to primitive types.

## `sizeof<Type>`

Outside annotation arguments, `sizeof<Type>` is an ordinary expression of type `usize`:

```omega
bytes := sizeof<MyStruct>;
word := sizeof<usize>;
```

It yields the target-specific in-memory size of the type according to Omega's layout rules. Pointer-sized primitive sizes therefore depend on the compilation target.

Inside `@layout`, only primitive types are accepted in `sizeof<...>`.

## `@inline`

Accepted forms:

```omega
@inline              # same as always
@inline(always)
@inline(never)
```

`@inline` is a hint, not a semantic guarantee. A backend that cannot enforce the requested behavior may warn rather than changing program semantics. The current backend limitation is tracked in [`../issues/language-limitations.md`](../issues/language-limitations.md).

## `@mangling`

Accepted forms:

```omega
@mangling(enabled)
@mangling(disabled)
@mangling(force = "exact_symbol")
```

- `enabled` uses normal Omega mangling.
- `disabled` uses the bare function/binding name. It is rejected on methods and generic functions.
- `force = "..."` uses the exact non-empty linker symbol. It is allowed on methods, but rejected on generic functions because all instantiations would otherwise collide.

`@mangling` also applies to `foreign` bindings and direct foreign functions (see [`foreign-function-interface.md`](foreign-function-interface.md)), where the *default* -- with no explicit `@mangling(...)` written -- is `disabled` rather than the ordinary-function default of `enabled`. Writing `@mangling(enabled)` on a foreign item is how it opts back into normal Omega symbol construction; this is required for a generic foreign definition, since a bare disabled name cannot distinguish instantiations.

A compilation must diagnose duplicate final linker symbols rather than relying on linker/backend failure.

## `@naked`

Accepted forms:

```omega
@naked
@naked()
```

`@naked` takes no arguments; `@naked(...)` with any argument is an error. It marks a function/method as having a raw, self-owned body and machine-level control flow instead of an ordinary Omega body -- full contract in [`functions.md`](functions.md#naked-functions).

`@naked` is rejected together with any `@inline` mode on the same declaration, regardless of which annotation is written first.

## `@suppress(warning_name, ...)`

```omega
@suppress(unused_import, inline_not_enforced)
```

Each argument is a bare warning name. `key = value` arguments are invalid for `@suppress`.

The names are intentionally not validated for existence: an unknown warning name is harmless. Suppression applies to warnings in the annotated item's defined scope according to each warning's documented scope.
