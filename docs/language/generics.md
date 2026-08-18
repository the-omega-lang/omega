# Generics

This chapter is normative for current Omega language behavior. Known implementation limitations are tracked under [`../issues/`](../issues/).

Omega generics are monomorphized: each concrete type-argument combination has concrete semantics and layout rather than being type-erased.

```omega
struct Node<T> {
    exposed value: T;
    exposed next: *Node<T>;
}

sum<T>(a: T, b: T) => T { a + b }
```

## Generic parameter lists

Generic parameters follow the declaration name:

```omega
f<T, U: SomeSpec>(a: T, b: U) => void { ... }
```

A parameter may have a spec bound and/or a default type. Bounds use `+` for conjunction:

```omega
f<T: A + B>(x: T) => void { ... }
```

A concrete instantiation must satisfy every declared bound. Spec aliases in bounds mean the conjunction of their member specs; see [`specs-and-conformance.md`](specs-and-conformance.md).

Unbounded generic code may call operations that successfully resolve for every concrete instantiation actually formed; this is Omega's current duck-typed behavior for unconstrained generics. Adding a spec bound makes the required capability nominal and validates the corresponding conformance.

## Function type inference

Ordinary generic function calls infer type arguments; there is no Rust-style turbofish call syntax.

```omega
sum(1, 2);       # T inferred as i32
```

Inference uses call arguments and the surrounding expected result type. Constraints are accumulated from left to right, with an already-established expected/result constraint taking precedence over later adaptable literals.

An explicitly typed expression or suffixed numeric literal does not silently change type to satisfy an incompatible expectation.

## Generic aggregate inference

Generic struct/union/enum construction can infer owner type arguments from:

1. the surrounding expected type, when it names the same declaration;
2. aggregate/variant field initializer types;
3. for a static function called through an owner with omitted type arguments, the static function's arguments when they constrain the owner parameters.

Examples:

```omega
opt := Option::Some { value = 42u32; };  # Option<u32>
none : Option<i32> = Option::None;        # expected type supplies T
pair := Pair { a = 3; b = 4; };           # fields supply type args
pair2 := Pair::new(7, 8);                  # static call can infer owner args
```

If required owner parameters remain unknown, the expression is invalid rather than receiving arbitrary defaults (unless declared generic defaults apply as described below).

Current limitations around overloaded static candidates and independent static-function generics are tracked in [`../issues/language-limitations.md`](../issues/language-limitations.md).

## Defaults

A generic parameter may declare a default:

```omega
struct List<T = i32> { ... }
struct Pair<A, B = A> { ... }
```

Defaults may refer to earlier parameters in the same list. Once a parameter has a default, every parameter after it must also have a default; defaults form a trailing suffix.

Omitted trailing arguments are filled from defaults after earlier arguments have become concrete. If every parameter has a default, the declaration may be referenced without `<...>` where otherwise unambiguous.

Explicit type arguments form a positional prefix:

```omega
Pair<u64>    # A = u64, B defaults to A => u64
```

## Defaults and function-call inference

For a function call, constraints are resolved broadly in this priority order:

1. surrounding expected result type;
2. concrete information already supplied explicitly or by earlier arguments;
3. a declared generic default when the generic is still unknown at the point it is needed;
4. inference from compatible argument values/aggregate fields.

Example:

```omega
add<T = u64>(a: T, b: T) => T { a + b }

x := add(10, 20);       # T = u64 by default
y := add(10u32, 20);    # T = u32; second literal adapts
z := add(10, 20u32);    # invalid if T was already fixed incompatibly
```

Expected result types can infer generics that occur in a return type:

```omega
lowest<T: Bounded>() => T { T::min() }
x : i32 = lowest();
```

If a generic appears nowhere that supplies information and has no applicable default, inference fails.

## Generic specs and conformances

Specs, conformances, structs, unions, enums, and functions may all participate in generic substitution where their grammar permits it.

The same generic spec may be implemented at different type arguments when the resulting required methods can coexist under ordinary overload rules. Blanket conformances may quantify over a target generic. Full rules are in [`specs-and-conformance.md`](specs-and-conformance.md).

## `spec S` parameter sugar

A parameter type written `spec S` (without `*`) behaves as its own anonymous generic parameter bounded by `S`. It is static dispatch, not a dynamic spec object. See [`specs-and-conformance.md`](specs-and-conformance.md) for its special meaning in spec return requirements.
