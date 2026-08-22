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

## Function types, calling conventions, and variadics

Function types use the same parameter/return spelling and always denote the implicit **Omega calling convention**:

```omega
handler : (i32, *u8) => bool;
```

`foreign(cc) (...) => T` is the same type shape with an explicit non-Omega calling convention (currently `c` or `sysv64`), used for function pointers/bindings that cross a foreign ABI boundary. See [`foreign-function-interface.md`](foreign-function-interface.md) for the full `foreign` syntax and semantics.

A trailing `...` is permitted only in a function type whose convention supports variadics (`c`, and `sysv64` on its supported targets), most commonly a `foreign(c)` declaration:

```omega
shared foreign(c) printf(format: *u8, ...) => i32;
```

An ordinary Omega-convention function type can never be variadic -- neither a function definition nor a plain `(...) => T` type may declare `...`. See [`foreign-function-interface.md`](foreign-function-interface.md) for the C default argument promotions applied to a variadic `foreign(c)` call's trailing arguments.

## `defer`

`defer statement` schedules the statement to execute when the enclosing function exits. It is function-scoped rather than block-scoped. See [`control-flow-and-operators.md`](control-flow-and-operators.md).

## Naked functions

`@naked` marks a function/method whose implementation is entirely a single `asm` statement, with no Omega-generated prologue, epilogue, parameter materialization, local frame, implicit return, or other runtime body instruction. See [`annotations-and-sizeof.md`](annotations-and-sizeof.md#naked) for the annotation form and [`inline-assembly.md`](inline-assembly.md#naked-functions) for the asm-side exception.

```omega
@naked
get_magic() => i32 {
    asm() => {
        mov eax, 123
        ret
    }
}
```

- The signature (parameters, receiver, return type) is unchanged: it is lowered through the same ABI as an ordinary function and remains the caller-facing contract for type checking and calls. `@naked` does not add calling-convention syntax and does not change Omega's ABI.
- After macro expansion, a naked function's body must contain exactly one `asm(...) => { ... }` statement and no other statement and no tail expression. Any other shape is rejected as an invalid naked body.
- Parameters (including a receiver) are ABI-only inside a naked function: Omega creates no parameter locals/places for them, does not warn that they are unused, and provides no operand-binding shortcut for them. A `$param` in the naked asm body is valid only if some descriptor in that same `asm` actually binds that name.
- `reg(...)` descriptors are forbidden inside a naked function's `asm`; only `const(...)` and `clobber(...)` are allowed, matching the ordinary asm descriptor rules otherwise.
- The naked asm owns control flow: it may contain the target return instruction, loop forever, tail-jump, or otherwise alter/restore the stack, unlike ordinary inline asm. Omega does not parse the body to prove it returns or returns the declared value; a naked function's `=> T` or `=> never` contract is enforced by the programmer, not the compiler.
- `@naked` is enforced (unlike the advisory `@inline`) and is rejected together with any `@inline` mode on the same declaration.
- `@naked` functions with Omega statements/locals/`defer`/`return` in the body, or with `reg(...)` in the naked asm, are rejected.
