# Functions

```
fibo(n: i32) => i32 {
    if n <= 1 { return n; }
    fibo(n - 2) + fibo(n - 1)
}

print_any(thing: *u8) => void { ... }
print_any(thing: u32) => void { ... }        # overload
print_any(thing: f64) => void { ... }

sum_generic<T>(a: T, b: T) => T { a + b }
process<T: Animal>(value: T) => void { value.make_sound(); }
```

`name(params) => ReturnType { body }` — no `fn`/`func`/`def` keyword at
all; a bare identifier in item position followed by `(` is what marks a
function declaration. Struct/enum/union **methods** use the exact same
grammar plus a leading self-mode parameter (`self`/`mut self`/`*self`/
`*mut self`) — see [structs & unions](04-structs-and-unions.md) and
[variables & mutability](02-variables-and-mutability.md) for the
self-by-value system and call-site auto-adaptation. A function with no
self-mode declared inside a struct/enum/union body is a *static* method
(`Type::function(...)`), not a member function.

## Return value

The function body's own tail expression (no trailing `;`) is the implicit
return value, exactly like a block expression anywhere else in the
language — `return` is only needed for an early exit. `void`-returning
functions commonly end in a statement instead, needing neither.

## Generics

`<T, U: Bound, ...>` directly after the name. Unlike a struct's, a
function's generics are **never given explicit arguments at a call site**
— they're deduced entirely from the call's own argument types. See
[generics](06-generics.md) for the monomorphization model this relies on
and its confirmed deduction gaps (a generic struct/enum-typed argument
can't currently be unified against).

## Overloading

Multiple functions (or methods) may share a name, disambiguated by
parameter count and type — including using literal-inference "cost" as a
tiebreaker (an unsuffixed literal argument can fit several candidates at
different cost; the minimum-cost viable candidate wins). Zero viable
candidates is `NoMatchingOverload`; a tie at the minimum is
`AmbiguousOverload` — never a silent guess. A bare, uncalled reference to
an overloaded name (`f := print_any;`) is ambiguous by default; it only
resolves if the context supplies an expected function type that
structurally matches exactly one candidate (`f : (u32) => void =
print_any;`).

A **module-qualified overloaded reference through a named import alias**
has its own, deliberately narrower visibility rule — see
[visibility](07-visibility.md)'s "Overloaded-candidate visibility" section.

## Variadic functions exist *only* for C interoperability — never in pure Omega

```
extern puts : (s: *u8) => i32;
extern printf : (s: *u8, ...) => i32;
```

`...` (trailing, after at least the closing comma) is grammar that exists
**exclusively** inside a `Type::Function` — i.e. an `extern` declaration's
type, or a function-type type-annotation — never inside an ordinary
function/method *definition*. This isn't a semantic rule bolted on
afterward; it's **structural**: `parse_function_definition` (the grammar
every hand-written `.omg` function and method goes through) has no `...`
production in it at all, and `FunctionDefinitionStmt::function_type()`
hardcodes `is_variadic: false` unconditionally. There is no code path by
which a function written in Omega can ever become variadic — the only way
a `ResolvedFunctionType` with `is_variadic: true` can exist at all is by
resolving an `extern` declaration's own type.

**Why**: variadics are a pre-ABI-stabilized C calling-convention feature
(`va_list`, no type safety, no way to know argument count from the callee
side alone) — exactly the shape needed to call `printf`/`puts` and nothing
else. Omega's own overloading (above) already covers the "one name,
several argument shapes" use case in a fully type-checked way, so there's
no expressiveness gap this restriction leaves open for native code; it
only ever matters at the C boundary.

**Mechanically**: `is_variadic: true` forces the function's call
convention to `SystemV` specifically (`make_function_sig`) — the one place
this codegen deviates from its own uniform internal calling convention, and
exactly the deviation needed to be binary-compatible with a real C
variadic callee. A variadic extern can only ever be *called*, never
implemented — nothing in the compiler ever generates a body for one (see
[modules & linkage](10-modules-and-linkage.md) — extern declarations are
scanned, not compiled).

## Return-value ABI: hidden struct-return pointer

A return type that flattens to more than the platform's small-value leaf
budget is passed via a hidden struct-return (`sret`) pointer parameter
instead of real return registers — decided once, in `make_function_sig`,
consulted identically by both a function's own definition and every call
site, so the two always agree. This is invisible at the Omega source
level; it only matters when reasoning about generated IR/assembly directly
(`--emit=ir`/`--emit=asm`).

## `defer`

See [control flow](03-control-flow.md) — `defer` schedules a statement to
run when the **enclosing function** returns (not the enclosing block),
Omega's only structured cleanup mechanism.

## Caveats

- The two confirmed variadic-argument `f64` codegen bugs (a forwarded
  `f64` parameter, and an `f64` read from an enum body field) both
  specifically manifest through this `SystemV`-call-convention path — see
  [primitives](01-primitives.md).
- A deeper `module::Type::function(...)` static-call path (through more
  than one level of module qualification) resolves without overload
  disambiguation at all — a documented, narrow gap distinct from the
  ordinary locally-visible-type overload path described above.
