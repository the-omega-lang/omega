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

A generic member is reached the same way once something determines its arguments -- an expected function type, or arguments written on the function segment. See [Selecting a function value](#selecting-a-function-value).

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

A generic declaration has no signature until its generic arguments are determined. A call determines them by inference, as described below. An **expected function type** determines them too, so a generic declaration may also be named uncalled wherever one is known: `member : (*Holder, u8) => u8 = Holder::self::echo;` instantiates `echo` with `T = u8` and yields that instantiation's one address. Without an expected function type, `Holder::self::echo` still names nothing, because nothing determines its arguments. Generic member/static functions are specified in [`generics.md`](generics.md).

## Overloading

Several functions or methods may share a name. A call is resolved using the argument count and argument types, including literal-adaptation cost.

Selection proceeds in this order:

- **Applicability.** For each candidate, inference uses the call's written
  generic arguments, expected result type, and written arguments under the
  rules in [`generics.md`](generics.md). A candidate whose parameter types
  cannot be determined or do not accept the arguments is not applicable.
  Neither is one whose declared bound set a written selector does not name, or
  whose bounds these arguments cannot prove. An inapplicable candidate never
  blocks an applicable one, so a call may reach a candidate that costs more
  than one that does not apply.
- **Cost.** Among applicable candidates, keep those at the minimum
  adaptation cost.
- **Preference.** If more than one remains, a concrete declaration beats a
  generic one. Among generics, let `U(c)` be the set of positions at which the
  caller wrote a **plain type argument** and candidate `c` declares no bounds;
  `a` beats `b` only when `U(a)` strictly contains `U(b)`. Selector positions,
  positions the caller did not write, and `comp` positions contribute nothing.
- **Result.** Exactly one surviving candidate is the selection; none is a
  no-match error, and more than one is ambiguous.

Nonempty bound sets are never ranked against each other. With `f<T: A>` and
`f<T: B>` declared and a type implementing both, `f<M>()` is ambiguous, and
adding `f<T: A + B>` does not resolve it -- an explicit selector does. With
`f<T>` and `f<T: A>` declared, `f<M>()` prefers the unbounded declaration
because the caller wrote a plain type argument there, while `f<M: A>()` selects
the bounded one and an inferred `f(M{})` is ambiguous.

An inapplicable candidate produces no diagnostics of its own. Deciding
applicability reads a candidate's declared bounds and resolves any default it
needs to reach them, but never analyzes its body, and only the selected
declaration is instantiated. Generic defaults do not establish applicability:
matching an argument never consults a default, though a generic parameter that
no argument determines still takes its default. A bound that is merely
unproven makes a candidate inapplicable; a malformed declaration or a
cyclic conformance proof remains a real error. An unsatisfied bound on a
declaration a selector explicitly named is an error, because the selector
already said which declaration was meant.

Preference compares declaration origin and what the caller wrote. It does not
compare parameter structure: `f<T>(x: T)` and `f<T>(x: *T)` are both unbounded
and remain ambiguous when both match at the minimum cost. Generic declarations
with structurally identical parameter types and required bound sets are
redeclarations, even if their generic parameters have different names. They are
rejected at their declarations, independently of calls. More than one
declaration may survive the same exact selector -- including bounds that become
identical only after substitution -- and an unresolved tie is then ambiguous.

Declaration order never affects which candidate is selected.

**Adapting a literal is a last resort, not a default.** A candidate that accepts
the arguments as written always beats one that only becomes viable by adapting a
literal, because the first costs nothing and the second does not. Adaptation
happens when there is no alternative, not because a wider parameter was declared:

```omega
f(a: i32) => void { ... }
f(a: i64) => void { ... }

f(10);      # selects 'f(a: i32)' -- '10' is already 'i32'; 'i64' would cost an adaptation
f(10i64);   # selects 'f(a: i64)' -- written as 'i64', so that candidate now costs nothing
```

With only `f(a: i64)` declared, `f(10)` does adapt, because nothing else can
accept the call. See [`types-and-primitives.md`](types-and-primitives.md) for a
literal's own default type.

The cost gate also applies when a candidate is generic:

```omega
f(a: i64) => void { ... }
f<T>(a: T) => void { ... }

f(10);      # selects the generic with T = i32: no adaptation is needed
f(10i64);   # selects the concrete declaration: both cost zero, so specificity decides
```

### Selecting a function value

An uncalled reference selects one declaration using the **expected function type**, which it must match exactly. Generic candidates participate: a generic declaration is instantiated with whatever arguments give it that signature.

```omega
thing(a: i32) => void { ... }
thing<T>(a: T) => void { ... }

a : (i32) => void = thing;        # the concrete declaration
b : (i32) => void = thing<i32>;   # the generic declaration, with T = i32
c : (u32) => void = thing;        # the generic declaration, with T = u32
thing(10u32);                     # a call: the generic, with no conversion
```

The rules are:

- Generic arguments written on the **function** segment (`thing<i32>`, `thing<spec A>`) restrict selection to generic declarations. A concrete declaration of the same name is excluded, whether or not a generic one then matches. They are a positional prefix, bound as at a call; the rest are inferred from the expected type.
- Matching is exact. Parameter count, parameter types, return type, calling convention, and variadic status must all be identical after substitution, at every depth: no literal adaptation, pointer-mutability weakening, anonymous-enum injection, receiver adaptation, or generated adapter applies. This is stricter than a call, which may pay a conversion cost for an argument; a value has nothing to pay it with. Parameter descriptors are not part of a function type and so never affect selection.
- A generic parameter the expected signature does not mention takes its declared default. A default never establishes a match, exactly as at a call.
- A candidate must also be applicable: a written selector must name its declared bound set exactly, and its bounds must be provable. As at a call, an inapplicable declaration never blocks another one.
- Among applicable exact matches, preference decides as it does for calls: a concrete declaration beats a generic one, and among generics the unbounded-position rule applies to whatever plain type arguments the reference wrote. Parameter structure is not a tie-breaker, so equally preferred or incomparable matches are ambiguous. Only the selected declaration is instantiated.
- With no expected function type, an uncalled reference still excludes generic declarations, because nothing determines their arguments. Written generic arguments are the exception: `f<i32>` needs no expected type when they leave exactly one declaration whose remaining parameters its own defaults complete.

A value and a call that reach the same declaration with the same generic arguments share one instantiation, and therefore one address, however each spelled the selection.

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
- The naked asm owns control flow: it may contain the target return instruction, loop forever, tail-jump, or otherwise alter/restore the stack, unlike ordinary inline asm. Omega does not parse the body to prove it returns or returns the declared value; a naked function's `=> T` or `=> never` contract is enforced by the programmer, not the compiler. Nothing is inserted into the naked body itself — but a naked `=> never` function is an ordinary callee, so an ordinary Omega caller still gets the call-site guard described under [`never` in `types-and-primitives.md`](types-and-primitives.md#never), and a naked body that returns is reported there rather than inside itself.
- `@naked` is enforced (unlike the advisory `@inline`) and is rejected together with any `@inline` mode on the same declaration.
- `@naked` functions with Omega statements/locals/`defer`/`return` in the body, or with `reg(...)` in the naked asm, are rejected.
