# Foreign-function interface

Omega supports explicit references to externally defined functions and explicit control over linker symbols. The language does not assume libc exists; C interoperability is a capability, not a runtime requirement.

## Extern function declarations

An external function declaration gives a name a function type without an Omega body:

```omega
shared extern malloc : (size: usize) => *mut u8;
```

Grammar:

```ebnf
extern-declaration = [ visibility ], "extern", identifier, ":", function-type, ";" ;
```

The declaration is type-checked like a callable function value. Calls use ordinary Omega call syntax.

Current Omega does not define storage semantics for non-function extern data declarations; that limitation is recorded in [`../issues/known-issues.md`](../issues/known-issues.md).

## Variadic calls

`...` exists only for foreign/C-style variadic function types. Pure Omega function definitions are not variadic.

```omega
shared extern printf : (format: *u8, ...) => i32;
```

Arguments before `...` are checked against their declared parameter types. Trailing variadic arguments use the FFI promotion/ABI behavior implemented for the target. A current float-varargs bug is tracked in [`../issues/known-issues.md`](../issues/known-issues.md).

## Symbol naming

Ordinary Omega functions use Omega's deterministic mangling scheme so module/type/generic identities do not collide across separately compiled packages.

Function annotations can override that behavior:

```omega
@mangling(disabled)
raw_add(a: i32, b: i32) => i32 { a + b }

@mangling(force = "exact_symbol")
entry(a: i32) => i32 { a }
```

- `disabled` uses the bare function name. It is rejected on methods and generic functions.
- `force = "..."` uses the non-empty string exactly. It may be used on methods but is rejected on generic functions.
- Two declarations resolving to the same final symbol are a compile error.

See [`annotations-and-sizeof.md`](annotations-and-sizeof.md) for annotation syntax.

## Program entry point

A function named `main` in the **root module** is Omega's program entry point. A function named `main` in another module is an ordinary mangled Omega function.

A root-module `main` must have the signature `main() => void` or `main() => never`: no parameters and no generics. This is enforced as a compile error. Command-line arguments and a return value doubling as a process exit code are both platform-dependent notions that do not hold on every target Omega runs on (embedded/freestanding targets in particular), so `main` stays a fixed, portable entry point. Reaching the end of a `void` `main` exits the program; a `never` `main` must diverge (for example by calling a platform-provided exit primitive).

The root-module `main` is **not** itself emitted under the platform's native entry-point symbol (for example the C `main` a hosted linker expects). It is emitted under a fixed internal symbol instead. Producing a runnable native program is the responsibility of the `plat` implementation being linked: a `plat` that wants to support runnable programs provides its own adapter under the platform's real entry-point symbol (a libc-hosted `plat` forces an ordinary function to the `main` symbol via `@mangling(force = "main")`; a freestanding target's `_start` calls the internal entry symbol directly) and calls into Omega's entry point from there. A `plat` that supplies no such adapter still links fine as a library-mode dependency.

There is no language-level library/program mode. A separately compiled package with no root-module `main` simply exports/references whatever its declarations require; the final linker decides how the object is used.

## Calling convention and aggregate ABI

Omega's internal calling convention is stable within the compiler's own separately compiled objects, but the current implementation does **not** yet promise platform C ABI compatibility for aggregates passed/returned by value. Scalars/pointers and explicitly compatible extern surfaces are the practical interop boundary today.

The missing full C-ABI aggregate convention is tracked in [`../issues/known-issues.md`](../issues/known-issues.md). A reimplementation must not assume Rust/C ABI aggregate lowering where Omega's language/ABI docs do not promise it.

## Gaps/glue versus FFI

`gap`/`glue` is Omega's package/platform capability mechanism, not an alternate spelling for arbitrary C externs. A gap declares a capability contract and glue supplies an Omega implementation under the matching symbol contract. See [`gaps-and-glue.md`](gaps-and-glue.md).
