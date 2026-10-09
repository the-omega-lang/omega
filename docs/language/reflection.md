# Reflection

This chapter is normative for current Omega language behavior. Known implementation limitations are tracked separately under [`../issues/`](../issues/).

Reflection gives a program an explicit, read-only description of a type: its name, size, alignment, fields and their offsets, and an enum's tags and variants. It exists for table-driven code such as serializers and for runtime type information a program asks for by name. Nothing is attached to values, and nothing is emitted unless the program asks for a description.

## The `typeinfo` operator

```ebnf
primary = ... | "typeinfo", "<", type, ">" ;
```

`typeinfo<T>` is an expression of type `*core::reflection::TypeInfo` (immutable) whose value is the address of the table describing `T`. It belongs to the `sizeof<T>`/`alignof<T>` family (see [`annotations-and-sizeof.md`](annotations-and-sizeof.md)): `typeinfo` is a contextual keyword that commits to the operator only when `<` follows, so it stays an ordinary identifier elsewhere, and the operator works after generic substitution and through aliases. It needs no import. The result type is named by its absolute path, so a program's own `TypeInfo` never takes its place; a compilation without `core::reflection::TypeInfo` rejects the operator.

`typeinfo<T>` runs no code and emits nothing by itself. Its value is constant data the compiler reflects.

## Declarations

`core::reflection` declares the types a table is made of:

```omega
exposed struct TypeInfo {
    exposed name: *str;
    exposed path: *[]*str;
    exposed generic_args: *[]GenericArg;
    exposed size: usize;
    exposed align: usize;
    exposed kind: TypeKind;
}

exposed enum GenericArg {
    Type { exposed type: *TypeInfo; },
    Comp { exposed type: *TypeInfo; exposed bits: u64; };
}

exposed struct FieldInfo {
    exposed name: *str;
    exposed offset: usize;
    exposed type: *TypeInfo;
    exposed visibility: Visibility;
}

exposed enum Visibility { Hidden, Shared, Exposed; }

exposed enum Primitive {
    Void, Never, Bool, Char, I8, I16, I32, I64, ISize,
    U8, U16, U32, U64, USize, F32, F64;
}

exposed enum CallingConvention { Omega, C, SysV64; }

exposed struct SpecInfo {
    exposed name: *str;
    exposed path: *[]*str;
    exposed generic_args: *[]GenericArg;
}

exposed struct VariantInfo {
    exposed name: *str;
    exposed tag: u64;
    exposed prototype: *u8;
    exposed fields: *[]FieldInfo;
}

exposed enum TypeKind {
    Primitive { exposed primitive: Primitive; },
    Pointer { exposed pointee: *TypeInfo; exposed mutable: bool; },
    UnknownSizeArray { exposed element: *TypeInfo; exposed mutable: bool; },   # *[?]T
    Array { exposed element: *TypeInfo; exposed length: usize; },              # [N]T
    Slice { exposed element: *TypeInfo; exposed mutable: bool; },              # *[]T
    Str { exposed mutable: bool; },                                            # *str
    Struct { exposed fields: *[]FieldInfo; },
    Union { exposed fields: *[]FieldInfo; },
    Marker,
    Enum {
        exposed tag_field: FieldInfo;
        exposed header: *[]FieldInfo;
        exposed shared: *[]FieldInfo;
        exposed variants: *[]VariantInfo;
    },
    AnonymousEnum { exposed tag_field: FieldInfo; exposed members: *[]VariantInfo; },
    Function {
        exposed params: *[]*TypeInfo;
        exposed ret: *TypeInfo;
        exposed variadic: bool;
        exposed convention: CallingConvention;
    },
    SpecObject { exposed specs: *[]SpecInfo; exposed mutable: bool; };
}
```

The field and variant names above are part of the contract; programs read tables through them.

## What a table describes

`typeinfo<T>` is the address of an immutable `TypeInfo` describing `T`. Every `*TypeInfo` inside a table is itself such an address, so a program walks from a type to the types it mentions, including back to itself through a recursive type such as `struct Node { next: *Node; }`.

### Names

For a **named type** — a struct, union, marker, or named enum — `name` is the declared name without generic arguments, `path` is the declaring module path, which begins with the package name, and `generic_args` lists the instance's generic arguments in declaration order.

For **every other type**, `name` is the type's spelling as a diagnostic would print it (for example `*Node`, `[4]u8`, `enum *str | u16`) but without function parameter descriptors at any depth, so `(x: i32) => void` and `(y: i32) => void`, which are one type, are both named `(i32) => void`. `path` and `generic_args` are empty. Equal types always have equal names, under `comp` and at run time. A name is presentation only: identity never compares it for these types.

A type argument is `GenericArg::Type`. A `comp` argument is `GenericArg::Comp`: `type` describes the parameter's declared type, and `bits` holds the value as two's complement truncated to that type's width, with `bool` as `0` or `1` and `char` as its code point. So the `N` of `Signed<-1>`, declared `comp N: i8`, has `bits` `255`.

### Size, alignment, and offsets

`size` is `sizeof<T>` and `align` is `alignof<T>` (see [`annotations-and-sizeof.md`](annotations-and-sizeof.md)) for the target being compiled. Every `offset` is a byte offset from the start of a value of the described type, following the layout rules of [`types-and-primitives.md`](types-and-primitives.md#layout) and [`enums-and-pattern-matching.md`](enums-and-pattern-matching.md).

### Kinds

- The primitive types are `Primitive`. `*T` and `*mut T` are `Pointer`, `*[?]T` is `UnknownSizeArray`, `[N]T` is `Array`, `*[]T` is `Slice`, and `*str` is `Str`, each recording its mutability or length and describing its element.
- A struct is `Struct`, and a union is `Union`, listing every field in declaration order — hidden and shared fields included, each with its declared `visibility`. Every union field has offset `0`.
- A marker type is `Marker`.
- A named enum is `Enum`. `tag_field` is a `FieldInfo` named `tag` describing the tag's real type and offset, with visibility `Exposed`; `header` lists the header fields after the tag, and `shared` lists the shared dynamic fields, each with its offset. Each `VariantInfo` gives the variant's name, its tag value as two's complement truncated to the tag type's width, its body fields with their offsets, and a `prototype`.
- An anonymous enum is `AnonymousEnum`. `tag_field` describes its `u32` tag. Each member, in canonical member order, is a `VariantInfo` whose `name` is the member type's spelling, whose `tag` is its member index, and whose single field, named `value`, holds the member at its offset.
- A function type is `Function`: `params` describes each parameter type in order, without its descriptor, `ret` the return type, `variadic` whether it takes a variadic tail, and `convention` its calling convention (`Omega` unless written `foreign(...)`).
- A spec object `*spec A + B` or `*mut spec A + B` is `SpecObject`: `mutable` records which, and `specs` lists each spec of the conjunction in its canonical order (see [`specs-and-conformance.md`](specs-and-conformance.md)), each as a `SpecInfo` with the spec's declared name, declaring module path, and generic arguments. Tables never list which specs a type implements.

A refined enum type such as `Shape::Circle` is described by its parent enum, because refinement never changes a value's storage.

### Variant prototypes

A variant's `prototype` addresses `sizeof<E>` bytes: a value of the enum with that variant's tag and header values written in and every other byte zero. A program constructs a value of a variant by copying the prototype and writing the shared and body fields at their offsets, without knowing the enum statically.

## Identity

Two tables describe the same type exactly when `core::cmp::Eq` says they do. `core::reflection` provides `meet Eq for TypeInfo`:

- tables of different kinds are unequal;
- named types are equal when their kind, `path`, `name`, and `generic_args` are equal, comparing a `comp` argument by its `bits` and type;
- every other type is equal when its components are: pointee or element, mutability, length, and an anonymous enum's members in order;
- functions are equal when their parameter types in order, return type, `variadic`, and `convention` are;
- spec objects are equal when their mutability is and their `specs` are pairwise, each by `path`, `name`, and `generic_args`.

No kind compares an unnamed type's `name`, so identity is made only of components.

`==` on two `*TypeInfo` compares addresses, as for any pointer. Equal addresses always describe the same type. The converse is not guaranteed: a program composed of separately loaded images may hold two copies of one type's table, so comparing addresses is a fast path, not a test of identity.

## Cost

A table, the strings and slices it points to, and its prototypes exist only when a program's runtime code uses an address that reaches them. A program that never writes `typeinfo`, or writes it only under `comp`, has no reflection data at run time, and no code exists for `typeinfo` at all. Types never carry a hidden pointer to their table, and tables never contain function addresses.

## Compile-time evaluation

`typeinfo` may be used under `comp`, and its tables can be read there like any other compile-time data:

```omega
comp NODE_SIZE := typeinfo<Node>.size;
comp SAME := Eq::equals(*typeinfo<Pair<i32>>, *typeinfo<Pair<i32>>);
```

`Eq for TypeInfo` is the identity test under `comp` as at run time. `==` and `!=` on two `*TypeInfo` still compare addresses there; because a compilation has exactly one table per type, that coincides with identity under `comp`.

Reading through a variant's `prototype` is not evaluable, because `comp` has no byte-level view of a value (see [`compile-time-evaluation.md`](compile-time-evaluation.md#unsupported-compile-time-operations)). Neither is reading a table through a pointer cast to another type, such as `*<*u8>typeinfo<i32>`.

## Rejected designs

- **A `typeof<T>` name.** It reads as "the type of an expression", which is what `typeof` means in C23 and Zig.
- **A compiler-implemented function `typeinfo<T>()`.** A description is constant data the compiler reflects, not code that runs, so it is in the category of `sizeof<T>`, not of a function. A function form also emits a real instance per described type and has to be imported.
- **Identity by mangled name or by address.** A mangled name is a backend artifact `comp` cannot produce, and an address is per image.
- **A table slot in every vtable.** It would cost every spec object, whether or not the program reflects.
