# Annotations, `sizeof`, and `alignof`

Annotations are explicit compiler directives written immediately before supported declarations.

```omega
@layout(pack = sizeof<usize>, align = sizeof<usize>)
struct Header { ... }

@inline
@suppress(inline_not_enforced)
fast() => void { ... }

@symbol(mangle = disabled)
raw_add(a: i32, b: i32) => i32 { a + b }
```

## Grammar

```ebnf
annotation = "@", identifier, [ "(", [ arg, { ",", arg } ], ")" ] ;
arg        = identifier | identifier, "=", value ;
value      = decimal-integer | "sizeof", "<", type, ">" | string-literal | identifier ;
```

Bare `@name` and `@name()` both carry zero arguments.

An identifier value is annotation syntax, not an expression and not a name lookup: it is the written word, and each annotation decides which words it accepts. `sizeof` followed by `<` is still the size form; anywhere else it is an ordinary identifier.

Recognized annotations are `layout`, `inline`, `naked`, `suppress`, and `symbol`. Duplicate use of the same annotation on one declaration is an error. Unknown annotation names are errors.

## Applicability

| Annotation | Allowed declarations |
|---|---|
| `@layout` | `struct`, `enum` |
| `@inline` | functions/methods |
| `@naked` | functions/methods, subject to restrictions below |
| `@suppress` | `struct`, `enum`, `union`, function/method, `import`, `spec` |
| `@symbol` | functions/methods, module-level storage bindings, `foreign` bindings/functions, subject to restrictions below |

Other item kinds do not acquire an annotation meaning merely because the generic `@...` syntax exists.

## `@layout(pack = n, align = n)`

Struct and enum layout defaults to:

```text
pack = 1
align = 1
```

Either key may be supplied independently and in either order. Each supplied value must be a non-zero power of two fitting `u32`. An `align` value must additionally be representable by the target's `usize`, which is a real restriction on 16-bit targets.

- `align` is a minimum **address** requirement, not only a relative field placement. It also decides trailing size rounding.
- `pack` controls field-grouping granularity: fields share a `pack`-sized chunk when they fit; otherwise placement advances to the next chunk boundary. A field larger than `pack` may begin at a chunk boundary rather than being rejected. `pack` never weakens an alignment.

A bare `@layout`/`@layout()` is equivalent to the defaults. Unions do not carry `@layout`, but they still inherit alignment from their members.

### Effective alignment

A type's **effective alignment** `A(T)` is the strongest requirement any part of it stored inline declares:

| Shape | Effective alignment |
|---|---|
| struct | maximum of its declared `align` and every field's `A` |
| named enum | maximum of its declared `align` and the `A` of its tag, header fields, shared dynamic fields, and every variant body field |
| anonymous enum | maximum of its tag's `A` and every member's `A` |
| union | maximum of every member's `A`, otherwise 1 |
| `[N]T` | `A(T)`, including when `N` is zero |
| primitives, pointers, function values, slices, strings, spec-object handles | 1 |

Primitives stay packed: Omega does not naturally align `i64` merely because a target would. Alignment follows **inline containment only** -- a pointer stores an address, so `A(*T)` is 1 no matter what `T` requires, and merely storing a pointer never imposes its pointee's alignment.

An explicit `align` smaller than a contained member's requirement -- including `align = 1` -- never lowers it.

Every address the compiler creates for a value of `T` satisfies `A(T)`: locals, globals, parameter homes, temporaries, hidden results, constant references, and slice backing storage. A raw pointer cast is still an unchecked address reinterpretation, and does not realign anything.

### Size rounding

Struct, enum, and union sizes round up to the effective alignment, so `sizeof<T>` is also a fixed-array element stride that keeps every element aligned:

```omega
@layout(align = 16)
struct Wide { exposed value: i64; }      # sizeof<Wide> == 16, alignof<Wide> == 16

struct Holder {
    exposed lead: u8;                    # offset 0
    exposed inner: Wide;                 # offset 16
}                                        # sizeof<Holder> == 32, alignof<Holder> == 16
```

An empty type keeps size zero while still requiring an aligned address; no storage bytes are invented for it. Padding bytes have no specified value.

An annotation layout value may be a decimal integer or `sizeof<Primitive>`; the `sizeof` form in `@layout` is deliberately restricted to primitive types.

## `sizeof<Type>`

Outside annotation arguments, `sizeof<Type>` is an ordinary expression of type `usize`:

```omega
bytes := sizeof<MyStruct>;
word := sizeof<usize>;
```

It yields the target-specific in-memory size of the type according to Omega's layout rules, including trailing padding. Pointer-sized primitive sizes therefore depend on the compilation target.

Inside `@layout`, only primitive types are accepted in `sizeof<...>`.

## `alignof<Type>`

`alignof<Type>` is an ordinary expression of type `usize` yielding the type's effective alignment:

```omega
bytes := sizeof<MyStruct>;
boundary := alignof<MyStruct>;
```

It parses exactly like `sizeof<Type>`: both are contextual identifiers that commit to the query form only when `<` follows, so a program may still use `sizeof` or `alignof` as an ordinary name elsewhere. Both are usable at runtime and in compile-time evaluation, and both work after generic substitution and through aliases.

`alignof` reports Omega's alignment, not a target's natural alignment, so `alignof<u64>` is `1`. Storage-less `void` and `never` report `1`. `alignof<*T>` is `1` regardless of `T`.

`alignof` is not accepted inside `@layout` arguments; the annotation grammar takes a plain integer or `sizeof<Primitive>`.

## `@inline`

Accepted forms:

```omega
@inline              # same as always
@inline(always)
@inline(never)
```

`@inline` is a hint, not a semantic guarantee. A backend that cannot enforce the requested behavior may warn rather than changing program semantics. The current backend limitation is tracked in [`../issues/language-limitations.md`](../issues/language-limitations.md).

## `@symbol`

`@symbol` decides two things about the symbol an item owns or refers to: the linker **name**, and whether that symbol is **visible outside the linked image** it belongs to.

```omega
@symbol(mangle = enabled)
@symbol(mangle = disabled)
@symbol(name = "exact_symbol")
@symbol(export)
@symbol(export = enabled)
@symbol(export = disabled)
@symbol(name = "exact_symbol", export)
```

| Parameter | Accepted values | Omitted |
|---|---|---|
| `mangle` | `enabled`, `disabled` | the item's naming default: `enabled` for ordinary items, `disabled` for `foreign` ones |
| `name` | a non-empty string without an embedded NUL | no exact name |
| `export` | `enabled`, `disabled`; bare `export` means `enabled` | the item's visibility default: `disabled` for ordinary items, `enabled` for `foreign` ones |

At least one parameter must be written: `@symbol` and `@symbol()` are errors. Only `export` may be written on its own; a bare `mangle`, `name`, `enabled`, or `disabled` is an error, as are unknown keys, a repeated key (including `export, export = enabled`), a value of the wrong kind, and a repeated `@symbol` on one declaration.

### Naming

- `mangle = enabled` uses normal Omega mangling.
- `mangle = disabled` uses the bare function/binding name. It is rejected on methods and on generic declarations.
- `name = "..."` uses that exact linker symbol. It is allowed on a nongeneric method, and rejected on a generic declaration -- including a method of a generic owner -- because every instantiation would otherwise collide on one symbol.

`name` already decides the symbol, so writing it together with `mangle` is an error whichever order the two appear in and whatever value `mangle` carries.

A compilation must diagnose duplicate final linker symbols rather than relying on linker/backend failure.

### Export

`export` decides binary visibility only. It never changes the linker name, and it is never Omega's source visibility: `exposed`, `shared`, `hidden`, and `reveal` decide what other Omega source may name, and they say nothing about the emitted symbol (see [`visibility.md`](visibility.md#binary-visibility-is-a-separate-decision)).

An **Omega-declared** item is hidden by default. A hidden symbol still links across source files and separately compiled objects of one linked image; it is only absent from what that image exports to other images. `export` marks it visible to other images as well.

A **`foreign`** item is the opposite, and needs no annotation to be so. Declaring an item `foreign` is already the statement that its symbol is looked up in, or defined for, something outside this compilation, so it defaults to crossing images just as it defaults to the exact name written in source:

```omega
shared foreign(c) malloc(size: usize) => *mut u8;   # resolved from libc; no annotation needed
```

`export = disabled` is what a `foreign` item writes when the opposite is true -- when the symbol it names really is resolved inside this image, and marking it hidden should be enforced.

`export` is valid on a method and on a generic declaration, where it applies to whatever instantiations are emitted; it does not cause an otherwise unused generic to be instantiated.

On a bodyless declaration, visibility describes the symbol being *referred to* rather than creating a definition. A reference to a definition Omega itself knows about -- including one in a separately compiled Omega package -- carries that definition's own visibility instead of a reference-site default.

**Portability.** `export` means the emitted symbol takes the backend's default visibility rather than hidden visibility. It is not a promise that the final binary retains the symbol, nor that the symbol appears in every platform's dynamic export table; those remain decisions of the target and the linker.

### On a module-level storage binding

A module-level binding that owns storage -- `name : T;`, `name : T = value;`, or `name := value;`, with or without `mut` and under any visibility -- accepts `@symbol` to name the symbol of that storage:

```omega
@symbol(name = "unmangled_symbol_with_default_value")
my_symbol : i32 = 10;
```

The naming default is `enabled`, so an unannotated global keeps its ordinary module-qualified Omega symbol. `mangle = disabled` uses the written identifier verbatim. The annotation selects a linker name and a binary visibility only: source lookup, visibility, mutability, type, layout, alignment, and initialization are unchanged, and the declaration still owns and initializes its own storage.

A stored value is data even when its type is a function type, so it always uses global symbol construction -- unlike a function-typed `foreign` binding, which names an external *function* symbol.

A `comp` binding has no storage and no linker symbol, so it does not accept `@symbol`; neither do locals, parameters, fields, aliases, or types. `@symbol` is also the only annotation a module-level storage binding accepts.

### On a foreign item

`@symbol` also applies to `foreign` bindings and direct foreign functions (see [`foreign-function-interface.md`](foreign-function-interface.md)). `foreign` is where both defaults fork: naming is `disabled` rather than the ordinary-function `enabled`, and visibility is exported rather than hidden.

Writing `@symbol(mangle = enabled)` on a foreign item is how it opts back into normal Omega symbol construction; this is required for a generic foreign definition, since a bare disabled name cannot distinguish instantiations. The two parameters stay independent: opting into mangling does not also opt out of crossing an image.

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
