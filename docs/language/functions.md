# Functions

This chapter is normative for current Omega language behavior. Known implementation limitations are tracked under [`../issues/`](../issues/).

## Declaration syntax

```omega
fibo(n: i32) => i32 {
    if n <= 1 { return n; }
    fibo(n - 2) + fibo(n - 1)
}

print_any(thing: *u8) => void { ... }
print_any(thing: u32) => void { ... }

sum<T>(a: T, b: T) => T { a + b }
```

A function definition has no `fn` keyword:

```ebnf
function = identifier, [ generic-parameters ],
           "(", [ parameter-list ], ")",
           "=>", return-type, block ;
```

Parameters are immutable bindings. Methods use the same syntax inside a nominal type, with one of the receiver forms defined in [`bindings-and-mutability.md`](bindings-and-mutability.md).

A function declared inside a struct, union, enum, marker, primitive block, or conformance block with no receiver is a static function and is called through its owner (`Type::name(...)`). A declaration with `self`, `mut self`, `*self`, or `*mut self` is an instance method.

## Return values

The final expression of a block, when not followed by `;`, is that block's value and therefore can be a function's implicit return value. `return expr;` exits the current function immediately with `expr` as the result.

A `void` function does not need a tail value. A function declared `=> never` must diverge; see [`types-and-primitives.md`](types-and-primitives.md).

## Generics

Generic parameters follow the function name:

```omega
process<T: Animal>(value: T) => void {
    value.make_sound();
}
```

Function type arguments are inferred from arguments and expected result context; ordinary function calls do not have an explicit turbofish-style type-argument syntax. Bounds and inference rules are specified in [`generics.md`](generics.md).

## Overloading

Several functions or methods may share a name. A call is resolved using the argument count and argument types, including literal-adaptation cost.

- If no candidate is viable, the call is invalid.
- If exactly one minimum-cost candidate exists, that candidate is selected.
- If multiple candidates tie at the minimum cost, the call is ambiguous and must be rejected.

An uncalled reference to an overloaded name is ambiguous unless an expected function type selects exactly one overload:

```omega
f : (u32) => void = print_any;
```

Visibility also participates in candidate selection; see [`visibility.md`](visibility.md).

## Function types and C variadics

Function types use the same parameter/return spelling:

```omega
handler : (i32, *u8) => bool;
```

A trailing `...` is permitted only in a function type and is used for C-compatible variadic declarations, most commonly `extern` declarations:

```omega
extern printf : (format: *u8, ...) => i32;
```

An Omega function definition cannot itself be variadic. Calls to a variadic extern apply the C default argument promotions to arguments in the variadic tail. See [`foreign-function-interface.md`](foreign-function-interface.md).

## `defer`

`defer statement` schedules the statement to execute when the enclosing function exits. It is function-scoped rather than block-scoped. See [`control-flow-and-operators.md`](control-flow-and-operators.md).
