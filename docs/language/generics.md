# Generics

This chapter is normative for current Omega language behavior. Known implementation limitations are tracked under [`../issues/`](../issues/).

Omega generics are monomorphized: each concrete generic-argument combination has concrete semantics and layout rather than being type-erased.

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

A generic parameter is either a **type parameter** or a **`comp` parameter**.

A type parameter may have a spec bound and/or a default type. Bounds use `+` for conjunction:

```omega
f<T: A + B>(x: T) => void { ... }
```

A `comp` parameter binds a compile-time value instead of a type. Its value type is mandatory, and it never takes bounds:

```omega
struct Buffer<comp N: usize, T> {
    exposed data: [N]T;
}
```

A `comp` parameter is a storage-less compile-time binding, exactly like a `comp` declaration: it is immutable, has no runtime representation, and is usable anywhere an ordinary `comp` value is legal while the instantiated declaration is analyzed -- as an array length, in a range, in further `comp` evaluation, in a condition. It never becomes a runtime parameter or a stored field, so an abstraction over a value costs nothing at run time. See [`compile-time-evaluation.md`](compile-time-evaluation.md).

A `comp` parameter's declared type must currently be an integer type (including `isize`/`usize`), `bool`, or `char`, whether written directly or reached through an alias. Every other type -- floats, pointers, strings, slices, aggregates, functions, enums -- is rejected, because a generic argument's value participates in type identity, query identity, and symbol identity and therefore needs a stable canonical equality and mangling rule.

A concrete instantiation must satisfy every declared bound. A bound that names an alias of a spec conjunction means the conjunction of its member specs, exactly as if `A + B` had been spelled at the bound; see [`specs-and-conformance.md`](specs-and-conformance.md) and [`aliases.md`](aliases.md).

An alias may also carry its own generic parameter list, making it a type alias template whose parameters are substituted into its right-hand side. Such parameters take bounds and defaults under the rules of this chapter, but they create no nominal identity: `Keyed<i32>` for `alias Keyed<V> = Pair<*str, V>;` *is* `Pair<*str, i32>`. A bare alias of a generic declaration forwards that declaration's arity, defaults, bounds, and inference unchanged.

Unbounded generic code may call operations that successfully resolve for every concrete instantiation actually formed; this is Omega's current duck-typed behavior for unconstrained generics. Adding a spec bound makes the required capability nominal and validates the corresponding conformance.

## Function type inference

Ordinary generic function calls infer their generic arguments.

```omega
sum(1, 2);       # T inferred as i32
```

Inference uses call arguments and the surrounding expected result type.

A call may also write generic arguments explicitly, using the same diamond syntax as any other generic application. There is no separate turbofish spelling; `::<...>` is not Omega syntax.

```omega
ptr_cast<u32>(p);        # first generic fixed, the rest inferred
sum<i32>(1, 2);
```

Written arguments are a **positional prefix** of the declaration's generic parameter list, bound left to right. For `f<A, B, C>`, `f<X>(...)` fixes `A = X` only, and `f<X, Y>(...)` fixes `A = X` and `B = Y`. Generic arguments are never named, skipped, or reordered.

Because the list is positional, each written argument is read as the kind its parameter declares. A bare path such as `SIZE` is a type where the parameter is a type parameter and a compile-time value where it is a `comp` parameter; a type supplied to a `comp` slot, or a value supplied to a type slot, is a kind error rather than a guess.

Fewer arguments than the declaration has parameters is legal for a call: the remainder is inferred or defaulted under the rules below, and only a remaining parameter that nothing supplies is an error. More arguments than the declaration has parameters is rejected as an arity error.

Explicit arguments have the highest authority. They are never re-chosen to satisfy an argument, an expected result type, or a generic bound; a conflict is reported against the written type by the ordinary argument, result, and bound checks. Constraints are accumulated from left to right, with an already-established expected/result constraint taking precedence over later adaptable literals.

An explicitly typed expression or suffixed numeric literal does not silently change type to satisfy an incompatible expectation.

An anonymous enum that already exists is an ordinary type here. Given `alias Errors = enum ParseError | IoError;` and a value `e: Errors`, calling `identity<T>(x: T) => T` with `e` infers `T = Errors`, and substituting `T = Errors` into `enum T | C` flattens to `enum ParseError | IoError | C`. What inference may **not** do is construct an anonymous enum that no written type established: two arguments of unrelated types `A` and `B` never unify a parameter `T` to `enum A | B`, and the members of an expected anonymous enum are never tried one at a time as inference candidates. See [`enums-and-pattern-matching.md`](enums-and-pattern-matching.md).

## Compile-time value arguments

A `comp` argument is written as a scalar literal -- a signed or unsigned integer, `bool`, or `char` -- or as a path naming a `comp` binding or an enclosing `comp` generic parameter:

```omega
comp size := 3;

a := Buffer<3, i32> { ... };
b := Buffer<size, i32> { ... };
```

Arbitrary expressions are not part of generic-argument syntax. A computed value is written once as an ordinary `comp` binding and passed by name; the full compile-time evaluator is what produces it.

The declared parameter type is authoritative. Each value argument is evaluated and then **canonicalized to that type**, and is accepted only when it is exactly representable there. `comp size := 3;` above is an ordinary `i32` binding, and `Buffer<size, i32>` is the same instantiation as `Buffer<3, i32>` because `3` is exactly representable as `usize`. A negative, out-of-range, or wrong-kind value is rejected -- never truncated, wrapped, or converted.

A runtime binding is never a `comp` argument, however obvious its initializer is:

```omega
not_comp := 42;
Buffer<not_comp, i32>     # rejected: 'not_comp' is a runtime binding
comp is_comp := 42;
Buffer<is_comp, i32>      # accepted
```

Two instantiations are the same type exactly when their generic arguments are equal, kind by kind. `Buffer<10, i32>` and `Buffer<11, i32>` are distinct types with distinct layouts and distinct monomorphized symbols; two spellings that canonicalize to the same value are one type.

## Inference of `comp` parameters

A `comp` parameter is inferred only from compile-time structural information -- never from a runtime value. Matching a declared `[N]T` against a concrete fixed array binds both parameters:

```omega
count<comp N: usize, T>(values: [N]T) => usize { N }

values: [5]i32 = [1, 2, 3, 4, 5];
count(values);      # N = 5, T = i32
```

Matching a written generic application against a concrete one binds each position by its own kind, so `Buffer<N, T>` against `Buffer<4, u8>` binds `N = 4` and `T = u8`. A `comp` parameter that neither an argument, a structural match, nor a default determines is an inference error, exactly like an undetermined type parameter.

## Generic aggregate inference

Generic struct/union/enum construction can infer owner generic arguments from:

1. the surrounding expected type, when it names the same declaration;
2. aggregate/variant field initializer types;
3. for a static function called through an owner with omitted generic arguments, the static function's arguments when they constrain the owner parameters.

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
struct Block<comp N: usize = 4, comp M: usize = N> { ... }
```

A default is written as the kind its parameter binds: a type for a type parameter, a compile-time value for a `comp` parameter. Defaults may refer to earlier parameters in the same list. Once a parameter has a default, every parameter after it must also have a default; defaults form a trailing suffix.

Omitted trailing arguments are filled from defaults after earlier arguments have become concrete. If every parameter has a default, the declaration may be referenced without `<...>` where otherwise unambiguous.

Explicit generic arguments form a positional prefix:

```omega
Pair<u64>    # A = u64, B defaults to A => u64
Block<2>     # N = 2, M defaults to N => 2
```

## Defaults and function-call inference

For a function call, constraints are resolved broadly in this priority order:

1. explicitly written generic arguments;
2. surrounding expected result type;
3. concrete information already supplied explicitly or by earlier arguments;
4. a declared generic default when the generic is still unknown at the point it is needed;
5. inference from compatible argument values/aggregate fields.

Every rule below the first applies only to a generic that the call left unwritten.

Example:

```omega
add<T = u64>(a: T, b: T) => T { a + b }

x := add(10, 20);       # T = u64 by default
y := add(10u32, 20);    # T = u32; second literal adapts
z := add(10, 20u32);    # invalid if T was already fixed incompatibly

w := add<u32>(10, 20);  # T = u32; the default no longer applies
```

Expected result types can infer generics that occur in a return type:

```omega
lowest<T: Bounded>() => T { T::min() }
x : i32 = lowest();
```

If a generic appears nowhere that supplies information and has no applicable default, inference fails.

## Generic member and static functions

A function declared inside a struct, union, or enum may declare its own generic parameters, under this chapter's rules, whether or not its owner is generic:

```omega
struct Pair<A> {
    exposed a: A;

    exposed with<B>(*self, other: Pair<A>, b: B) => B { b }
    exposed of<B>(b: B) => B { b }
}
```

A declaration's own parameters are in scope for its signature and body alongside the owner's parameters and `Self`, and shadow an owner parameter that spells the same name.

Instantiation identity includes both lists: the owner's generic arguments and the declaration's own. `Pair<i32>::self::with<u8>` and `Pair<u8>::self::with<u8>` are distinct monomorphizations with distinct symbols, and two calls that resolve to the same pair of argument lists share one instantiation.

Arguments are inferred from the call's written arguments and the expected result type exactly as for a top-level generic function. The receiver of `value.name(...)` is supplied by the instance syntax, so it is not one of the arguments inference reads; `Owner::self::name(receiver, ...)` writes it out as an ordinary argument instead. A call may fix a left-to-right prefix explicitly, on the member (`value.name<T>(...)`) or on the function segment of a type-qualified path (`Owner::name<T>(...)`, `Owner::self::name<T>(...)`). One path carries at most one written argument list, so an owner and its function cannot both be written explicitly in one call.

The `spec S` parameter sugar applies here as it does to a top-level function: `f(x: spec S)` is a declaration with an anonymous bounded generic parameter, and is therefore instantiated per argument type.

Because a generic declaration has no signature before its arguments are known, it does not participate in overload resolution and cannot be named uncalled; see [`functions.md`](functions.md). Positions this leaves unsupported are tracked in [`../issues/language-limitations.md`](../issues/language-limitations.md).

## Generic specs and conformances

Specs, conformances, structs, unions, enums, and functions may all participate in generic substitution where their grammar permits it.

The same generic spec may be implemented at different generic arguments when the resulting required methods can coexist under ordinary overload rules. Blanket conformances may quantify over a target generic. Full rules are in [`specs-and-conformance.md`](specs-and-conformance.md).

## `spec S` parameter sugar

A parameter type written `spec S` (without `*`) behaves as its own anonymous generic parameter bounded by `S`. It is static dispatch, not a dynamic spec object. See [`specs-and-conformance.md`](specs-and-conformance.md) for its special meaning in spec return requirements.
