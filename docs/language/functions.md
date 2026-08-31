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

A function declared inside a struct, union, enum, marker, primitive block, or conformance block with no receiver is a static function. A declaration with `self`, `mut self`, `*self`, or `*mut self` is an instance method.

A spec requirement uses the same optional generic-parameter syntax; see [`specs-and-conformance.md`](specs-and-conformance.md#generic-requirements) for its matching and dispatch rules.

## Associated-function namespaces

Every function-bearing type has **two independent associated-function namespaces**, selected by how a type-qualified path is spelled:

| Spelling | Selects |
| --- | --- |
| `Type::name` | receiverless (static) functions only |
| `Type::self::name` | receiver-bearing (member) functions only |
| `value.name(...)` | member functions only, with the receiver supplied implicitly |

`Type::name` never resolves a member function and `Type::self::name` never resolves a static one; each reports the other spelling instead of silently crossing over. A static and a member may therefore share a name *and* an effective signature:

```omega
struct Thing {
    exposed v: i32;

    exposed same(other: *Thing) => i32 { other.v }   # Thing::same
    exposed same(*self) => i32 { self.v }            # Thing::self::same
}
```

The `self` segment is contextual, not a reserved identifier. It opens the member namespace only directly after a resolved type and only when another segment follows, so `Type::self` alone still names a static function or enum variant literally called `self`, and a leading module-relative `self::...` path is unaffected. An enum variant lives in the ordinary namespace only: `Enum::self::Variant` does not name one.

The rule applies to every concrete implementation owner — structs, unions, named enums, markers, primitive blocks, and conformance implementations reached through a concrete type. An alias resolves to its type first, so it selects namespaces identically. Anonymous enums own no declarations and are unaffected.

Precedence between an inherent and a conforming declaration is unchanged, but applies **inside the selected namespace only**: an inherent static `foo` does not hide a conforming member `foo` from `Type::self::foo`, and an inherent member `foo` does not hide a conforming static `foo` from `Type::foo`.

Visibility is checked on the declaration the namespace selected, exactly as before. `Type::self::name` is not a visibility bypass.

## Unbound member function values

`Type::self::name` yields an **unbound ordinary function value**: the receiver becomes an explicit first parameter, and the declaration-only receiver form is gone from the type.

```omega
member : (target: *Thing) => i32 = Thing::self::same;
println$(member(&t));            # the receiver is an ordinary argument
println$(Thing::self::same(&t)); # the same call written directly
```

`same(*self) => i32` on `Thing` is exposed as a function type taking `*Thing`; `*mut self` becomes `*mut Thing`, and a by-value receiver becomes a `Thing` parameter. Taking the value captures no receiver, allocates nothing, and creates no closure, thunk, or calling-convention adapter — it is the same one code address the type declares (see [`strings-casts-arrays-and-slices.md`](strings-casts-arrays-and-slices.md#function-values-and-thin-raw-pointers)).

The implicit receiver adaptation of `value.name(...)` — auto-borrow, auto-deref, mutability checking — applies to instance syntax only. Calling the acquired value, or calling `Type::self::name(receiver, ...)` directly, passes the receiver as an ordinary argument with no adaptation.

The receiver parameter of a member value carries no descriptor. Descriptors are not part of function-type identity, so the value stores into `(*Thing) => i32` and `(target: *Thing) => i32` alike; every other part of the function type must still match exactly.

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

A function's generic arguments are inferred from arguments and expected result context, and a call may fix a left-to-right prefix of them explicitly with `f<T, ...>(...)`. Omega has no turbofish spelling; `::<...>` is not syntax. Bounds and inference rules are specified in [`generics.md`](generics.md).

A function declared inside a nominal type may declare generic parameters of its own, in either associated-function namespace:

```omega
struct Holder {
    exposed value: i32;

    exposed echo<T>(*self, thing: T) => T { thing }
    exposed make<T>(thing: T) => T { thing }
}

h.echo(1u8);                    # inferred
h.echo<u8>(1);                  # written on the member
Holder::make<u8>(1);            # written on the static
Holder::self::echo<u8>(&h, 1);  # the member as an unbound value's call
```

A generic declaration has no signature until a call determines its arguments, which has two consequences. It does not participate in overload resolution: two generic declarations sharing a name and a namespace are rejected where they are called, since nothing can rank them. And it cannot be named without being called, so `Holder::self::echo` is not an unbound member value; only an instantiated declaration has a single address. Generic member/static functions are specified in [`generics.md`](generics.md).

## Overloading

Several functions or methods may share a name. A call is resolved using the argument count and argument types, including literal-adaptation cost.

- If no candidate is viable, the call is invalid.
- If exactly one minimum-cost candidate exists, that candidate is selected.
- If multiple candidates tie at the minimum cost, the call is ambiguous and must be rejected.

An uncalled reference to an overloaded name is ambiguous unless an expected function type selects exactly one overload:

```omega
f : (thing: u32) => void = print_any;
```

**The two associated-function namespaces are separate overload domains.** A static and a member never participate in one overload set, are never compared for redeclaration, and adding an overload to one namespace cannot make the other ambiguous. Within the member namespace the existing rule still holds: receiver spelling alone is not a selector, so two members differing only in `self` versus `*self` are rejected. An uncalled `Type::self::name` selects among member overloads using the unbound function value type, receiver parameter included.

Visibility also participates in candidate selection; see [`visibility.md`](visibility.md).

## Function types, calling conventions, and variadics

Function types always denote the implicit **Omega calling convention**. Each ordinary parameter is written either as a bare `Type` or as `name: Type`, and the two forms may be mixed in one list:

```omega
handler   : (i32, *u8) => bool;
described : (code: i32, data: *u8) => bool;
mixed     : (i32, data: *u8) => bool;
```

A written parameter name in a function type is **optional descriptive metadata**, not a binding and not part of the type. The three types above are one type: they are mutually assignable, select the same overload from an expected-type reference, satisfy the same spec requirement, and hash and compare identically. Renaming or removing a descriptor never changes a type's identity, layout, ABI, or mangled symbols; a diagnostic may still render a type with whichever descriptors it happens to carry.

Function *declarations* are unaffected: a function, method, spec member, gap member, or glue member parameter is a binding and must still be written `name: Type`.

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
- `reg(...)` descriptors are forbidden inside a naked function's `asm`; only `comp(...)` and `clobber(...)` are allowed, matching the ordinary asm descriptor rules otherwise.
- The naked asm owns control flow: it may contain the target return instruction, loop forever, tail-jump, or otherwise alter/restore the stack, unlike ordinary inline asm. Omega does not parse the body to prove it returns or returns the declared value; a naked function's `=> T` or `=> never` contract is enforced by the programmer, not the compiler.
- `@naked` is enforced (unlike the advisory `@inline`) and is rejected together with any `@inline` mode on the same declaration.
- `@naked` functions with Omega statements/locals/`defer`/`return` in the body, or with `reg(...)` in the naked asm, are rejected.
