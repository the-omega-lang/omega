# Foreign-function interface

Omega supports explicit references to externally defined functions/data and explicit control over linker symbols. The language does not assume libc exists; C interoperability is a capability, not a runtime requirement.

`foreign` is the keyword for all of this. It keeps two independent facts separate everywhere in the language:

1. Whether a source item is a **foreign binding/definition** -- this controls default symbol mangling/linkage, not ABI.
2. The function type's **calling convention** -- this controls type identity and ABI lowering.

An ordinary Omega function type always uses the implicit Omega convention, whether or not it happens to be named by a `foreign` binding.

## Foreign bindings

`foreign name : Type;` binds an external symbol whose type is `Type`. Any function calling convention comes from `Type` itself and defaults to Omega for an ordinary function type:

```omega
shared foreign malloc_omega_abi : (size: usize) => *mut u8;
```

This is rare in practice -- most external functions use a non-Omega ABI, spelled explicitly (see "Direct foreign functions" below). A non-function `Type` binds an external data symbol; it becomes a real linker-visible global with no initializer/storage allocated in the current object.

`foreign(cc) name : Type` is rejected: a binding never applies a convention to its own type. Write the convention on `Type` directly instead:

```omega
shared foreign printf : foreign(c) (format: *u8, ...) => i32;
```

## Direct foreign functions

`foreign(cc) name(args) => T;` declares a foreign function directly, with `cc` selecting its calling convention:

```omega
shared foreign(c) malloc(size: usize) => *mut u8;
shared foreign(c) printf(format: *u8, ...) => i32;
```

`foreign name(args) => T;` (no `(cc)`) is the direct Omega-convention foreign-function declaration -- a foreign binding whose type happens to be an ordinary function type, spelled without the indirection of a separate `Type`.

A direct foreign function may have a body:

```omega
@mangling(enabled)
foreign(c) callback(a: i32, b: i32) => i32 {
	a * b
}
```

This is still a foreign item (its default mangling is `disabled`, not the ordinary-function default `enabled` -- see "Symbol naming" below); the body just makes it a definition Omega itself compiles and exports, rather than an external declaration.

## Foreign blocks

`foreign(cc) { ... }` groups foreign entries under one convention, applied only to direct function-signature entries; it is purely syntactic and carries no semantics beyond expanding to the equivalent flat declarations. Nested foreign blocks are not allowed.

```omega
foreign(c) {
	malloc(size: usize) => *mut u8;
	free(ptr: *u8) => void;
	fp : (i32) => void;              # stays Omega -- the explicit type is authoritative
	typed_fp : foreign(c) (i32) => void;  # explicitly C
}
```

A data entry (`name : Type;`) never receives the block's convention -- `cc` only applies where a direct signature (`name(args) => T;`) is written with no type of its own.

## Calling-convention function types

`foreign(cc) (...) => T` is type syntax, usable anywhere a type is expected (locals, fields, parameters, function pointers):

```omega
c_fp : foreign(c) (a: i32, b: i32) => i32 = callback;
```

Bare `foreign (...) => T` (no `(cc)`) is invalid at the type level: the ordinary type `(...) => T` already denotes the Omega convention, so there is nothing for a bare `foreign` to add.

Initial named conventions are `c` and `sysv64`. `sysv64` selects the AMD64 System V ABI explicitly and is only accepted on compatible `x86_64` targets; using it elsewhere is a compile error rather than a silent fallback. `c` and `sysv64` are distinct types even on a target where both currently lower to the same machine convention, and both are distinct from the ordinary Omega convention -- `foreign(c) (i32) => i32` and `(i32) => i32` never unify, compare equal, or coerce into each other.

Calling convention is part of complete function-type identity (equality, function-pointer assignment/coercion, indirect-call typing, mangled type identity). It is **not** an overload selector: two same-name functions with the same callable parameter types remain duplicate/redeclaration candidates regardless of any differing convention, because direct call syntax has no way to choose a convention.

## Variadic calls

`...` exists only for foreign conventions that support it; an ordinary Omega function type can never be variadic. `c` supports variadics; `sysv64` supports them on its accepted x86-64 targets. A convention that does not support variadic arguments on the current target rejects `...` at the declaration.

```omega
shared foreign(c) printf(format: *u8, ...) => i32;
```

Arguments before `...` are checked against their declared parameter types.

For a variadic `foreign(c)` call, the trailing arguments get the C default argument promotions (`float` -> `double`, integer types narrower than `int` -> `int`/`unsigned int`) before the call -- this is required C source-language interoperability, since LLVM lowers exactly the IR types it is given and does not invent C's promotion rules on its own. This promotion is specific to `foreign(c)`: a variadic `foreign(sysv64)` tail is passed using its actual lowered Omega types, with LLVM performing the target's own register/stack classification -- being variadic does not by itself mean "apply C promotions."

Variadic foreign function *definitions* (a body that reads its own variadic tail) are not yet supported; only declarations/calls are.

## Symbol naming

Ordinary Omega functions default to `@mangling(enabled)`: Omega's deterministic mangling scheme, so module/type/generic identities do not collide across separately compiled packages.

Foreign items (bindings and direct foreign functions/definitions alike) default to `@mangling(disabled)` instead: the bare source name is the linker symbol, matching how an external declaration usually needs to name an exact existing symbol. `@mangling(enabled)` opts back into ordinary Omega symbol construction (needed, for example, for a generic foreign definition, since a disabled bare name cannot distinguish instantiations); `@mangling(force = "...")` uses the given name exactly, foreign or not.

```omega
foreign(c) malloc(size: usize) => *mut u8;      # linker symbol: "malloc"

@mangling(enabled)
foreign(c) callback(a: i32, b: i32) => i32 { a * b }   # ordinary Omega symbol

@mangling(force = "exact_symbol")
foreign(c) entry(a: i32) => i32;                 # linker symbol: "exact_symbol"
```

- `disabled` uses the bare name. It is rejected on methods and on generic functions (foreign or ordinary).
- `force = "..."` uses the non-empty string exactly. It may be used on methods but is rejected on generic functions.
- Two declarations resolving to the same final symbol are a compile error, except the intentional gap/glue identity described in [`gaps-and-glue.md`](gaps-and-glue.md).

See [`annotations-and-sizeof.md`](annotations-and-sizeof.md) for annotation syntax.

## Program entry point

A function named `main` in the **root module** is Omega's program entry point. A function named `main` in another module is an ordinary mangled Omega function.

A root-module `main` must have the signature `main() => void` or `main() => never`: no parameters and no generics. This is enforced as a compile error. Command-line arguments and a return value doubling as a process exit code are both platform-dependent notions that do not hold on every target Omega runs on (embedded/freestanding targets in particular), so `main` stays a fixed, portable entry point. Reaching the end of a `void` `main` exits the program; a `never` `main` must diverge (for example by calling a platform-provided exit primitive).

The root-module `main` is **not** itself emitted under the platform's native entry-point symbol (for example the C `main` a hosted linker expects). It is emitted under a fixed internal symbol, `_omg_main`, declared in the runtime as `foreign _omg_main : () => void;` -- an Omega-convention foreign binding, since it is Omega code calling Omega code across a compilation-unit boundary, not a C-ABI boundary. Producing a runnable native program is the responsibility of the `plat` implementation being linked: a `plat` that wants to support runnable programs provides its own adapter under the platform's real entry-point symbol. A libc-hosted `plat` forces a `foreign(c)` definition to the `main` symbol (`@mangling(force = "main")`), making its C-facing ABI explicit; a freestanding target's `_start` calls the internal entry symbol directly. A `plat` that supplies no such adapter still links fine as a library-mode dependency.

There is no language-level library/program mode. A separately compiled package with no root-module `main` simply exports/references whatever its declarations require; the final linker decides how the object is used.

## Calling convention and aggregate ABI

Omega's internal calling convention is stable within the compiler's own separately compiled objects. For non-Omega conventions (`c`, `sysv64`), the current implementation supports the scalar/pointer/function-pointer boundary; it does **not** yet promise platform ABI compatibility for aggregates (structs/unions/enums) passed or returned by value across a `foreign` boundary of any convention -- that is rejected at compile time rather than silently miscompiled. Pointers to such data remain fine.

The missing full C/SysV aggregate classification is tracked in [`../issues/known-issues.md`](../issues/known-issues.md). A reimplementation must not assume Rust/C ABI aggregate lowering where Omega's language/ABI docs do not promise it.

## Gaps/glue versus FFI

`gap`/`glue` is Omega's package/platform capability mechanism, not an alternate spelling for arbitrary foreign declarations. A gap declares a capability contract and glue supplies an Omega implementation under the matching symbol contract. See [`gaps-and-glue.md`](gaps-and-glue.md).
