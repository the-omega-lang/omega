# Compile-time evaluation (`comp`)

This chapter is normative for current Omega language behavior. Known implementation limitations are tracked under [`../issues/`](../issues/).

`comp` requests ordinary Omega code to be evaluated during compilation. It does not create a separate class of functions: the same function may be called at runtime or from a `comp` expression.

```omega
add(a: i32, b: i32) => i32 { a + b }

comp SIZE := comp add(10, 20);
value := comp add(20, 30);
```

There are two independent uses:

- `comp expression` evaluates the expression at compile time and yields the resulting value.
- `comp name := initializer;` declares a storage-less compile-time binding. References to the binding denote its already-known value.

A `comp` binding is immutable. `mut comp name := ...` is invalid because a substituted value has no mutable runtime storage.

A non-`comp` binding may still have a `comp` initializer; in that case only the initializer is evaluated at compile time and the binding itself is an ordinary runtime place.

A local `comp` binding is an ordinary lexical binding for name-resolution purposes: it shadows an earlier binding of the same name and may itself be shadowed, by another `comp` binding or by a runtime one. See [`bindings-and-mutability.md`](bindings-and-mutability.md). Module-scope `comp` bindings are not shadowable, like every other module-scope declaration.

## Global bindings

Module-scope bindings may be storage-less `comp` values or real globals:

```omega
comp SIZE := comp add(10, 20);

value := 10;                 # compile-time-known initializer, real storage
mut count : i32 = 0;         # mutable real global
state : SomeType;            # zero-initialized real global
```

If a top-level binding has an initializer, that initializer must be compile-time-known. Literal/aggregate constant forms may satisfy this directly; actual computation such as arithmetic or a function call requires compile-time evaluation.

```omega
x := 10;                     # valid
name := "omega";            # valid
items := &[1, 2, 3];         # valid

x := 10 + 20;                # invalid without comp evaluation
x := make_value();           # invalid without comp evaluation
x := comp make_value();      # valid if evaluation succeeds
```

A top-level declaration without an initializer has real zero-initialized storage. `mut` controls whether that storage may subsequently be written.

## Evaluation model

The operand of `comp` is subject to the ordinary language's name resolution and type checking. If it is valid, execution occurs using Omega semantics and produces a compile-time value.

Current compile-time evaluation supports the ordinary operations needed by Omega's constant-producing code, including:

- arithmetic, comparisons, boolean operations, and casts;
- blocks, `if`, `match`, `while`, classic `for`, range `for`, and `loop`;
- `break`, `continue`, `return`, `defer`, and the try operator `?`;
- fixed arrays and compile-time slices;
- struct, union, marker, and enum construction and field access;
- indexing and supported slicing;
- `sizeof<Type>`;
- calls to ordinary named Omega functions, including generic/overloaded/cross-module calls after normal resolution;
- nested `comp` evaluation;
- addresses/references to compile-time data where the resulting value can be represented as immutable static data.

`defer` inside a compile-time function call follows the same function-scoped FILO execution rule as at runtime and runs after the function's return value has been determined. A failing `?` exits the evaluated function the same way an explicit `return` does, so its deferred statements still run.

A `comp` operand that itself exits the enclosing *runtime* function — a bare `?` under `comp`, for instance — has no frame to return into, so the evaluation fails with a diagnostic rather than producing a value.

## Unsupported compile-time operations

The following operations are not currently evaluable by `comp`:

- calling a `foreign` function;
- dynamic dispatch through `*spec S` / `*mut spec S`;
- indirect calls through a function-typed variable or field;
- reading a non-`comp` global from within compile-time evaluation.

Encountering one of these causes the `comp` evaluation to fail; it does not silently defer that portion to runtime.

Compile-time execution is also bounded to prevent non-terminating loops/recursion from hanging compilation. The current implementation limit is an implementation constraint rather than a language guarantee and is documented under [`../issues/compiler-limitations.md`](../issues/compiler-limitations.md).

## Addresses of `comp` values

A `comp` binding has no ordinary runtime storage. When immutable addressable storage is required—for example by `&VALUE`, a pointer-receiver method call, or slicing—the value may be materialized as immutable static data and the operation uses an address into that materialization.

```omega
comp DATA := comp make_data();
ptr := &DATA;
```

This materialization never grants mutability. Taking `&mut` of a `comp` binding or calling a `*mut self` method that would require writable storage is invalid.

Plain field access, indexing, and by-value method calls can use the compile-time value directly and do not require an addressable materialization.

## References created during compile-time evaluation

Within a `comp` evaluation, taking an immutable reference to compile-time data is allowed when the referenced data can become static immutable data in the final program. Such references may therefore appear inside compile-time-constructed aggregates.

Mutable references into compile-time-only data are not produced: compile-time promoted data is immutable.

## Intrinsically compile-time positions

Some language positions require compile-time values even without an explicit `comp` prefix. In those positions, an expression may be evaluated at compile time as required by the construct.

Examples include enum tag/header values and `&[...]` compile-time slice contents:

```omega
compute_default_limit() => i32 { 10 + 5 }

enum Setting(exposed limit: i32) {
    Default(compute_default_limit()),
}
```

Because the enum header position is intrinsically compile-time-only, no extra `comp` keyword is required there as long as the expression can be evaluated successfully.
