# Volatile access

This chapter is normative for current Omega language behavior. Known implementation limitations are tracked separately under [`../issues/`](../issues/).

A volatile access is a read or write the compiler promises to perform exactly as written: it is never removed, repeated, merged with another access, or reordered against another volatile access. It exists for memory whose reads and writes have effects the program cannot see in its own code: memory-mapped device registers, storage shared with an interrupt handler, and DMA descriptors.

Volatility is an operation on a pointer, not a property of a type or of storage. A volatile location is ordinary Omega storage, exactly as an atomic location is (see [`atomics.md`](atomics.md)); the same storage may be accessed volatilely in one place and ordinarily in another.

## Declarations

The two operations are declared in `core::volatile`:

```omega
exposed read_volatile<T>(location: *T) => T;
exposed write_volatile<T>(location: *mut T, value: T) => void;
```

These and `core::reflection::typeinfo` (see [`reflection.md`](reflection.md)) are the only functions in Omega without a body (see [`grammar.md`](grammar.md#functions)). The compiler supplies each instance's body. Recognition is by declaration identity: the module `core::volatile` and the exact name. A declaration there with one of these names must have exactly the shape above: `exposed`, no annotations, one type parameter with no bound and no default, the parameter names and types shown, and no body. Any other shape is rejected. A bodyless function anywhere else is rejected, including a generic one that is never instantiated.

Apart from where their bodies come from, they are ordinary generic functions. Name resolution, imports, aliasing, visibility, overloading, generic-argument inference, explicit generic arguments, and taking an instance as a function value all follow [`functions.md`](functions.md) and [`generics.md`](generics.md):

```omega
import core::volatile::read_volatile;
import core::volatile::write_volatile;

write_volatile(&mut register.control, 1u32);   # T = u32, inferred
status := read_volatile<u16>(&register.status);
read: (*u32) => u32 = read_volatile<u32>;
```

`write_volatile` requires a `*mut T` location. A `*T` is a type mismatch, since Omega has no implicit conversions.

## Access semantics

A call to `read_volatile` reads the `T` stored at `location` and returns it. A call to `write_volatile` stores `value` at `location`.

Each call performs its accesses exactly once. They are not removed, even when the result is unused or the stored value is never read. They are not repeated, not merged with neighbouring accesses, and not reordered against any other volatile access in the same execution context.

A `T` that is a primitive scalar (an integer, `isize`/`usize`, a float, `bool`, or `char`), a thin data pointer, or a function pointer is accessed in a single access when the target has an instruction of that width. Any other `T` is accessed as a sequence of narrower accesses. The split and its order are unspecified, and a program must not rely on one.

A zero-sized `T`, such as `void` or a `marker` type, performs no access. The call still completes normally.

## Alignment

`location` must be aligned for the access:

- for a primitive scalar, a thin data pointer, or a function pointer, its **natural alignment**: the larger of `alignof<T>` and `sizeof<T>`;
- for every other type, `alignof<T>`.

Omega primitives are packed by default (see [`annotations-and-sizeof.md`](annotations-and-sizeof.md)), so `alignof<u32>` is `1`, but a volatile `u32` access still assumes 4-byte alignment. As with atomics, a program meets this with `@layout(align = n)` on the type owning the location. A device register map normally states this anyway. A single-field struct is not a scalar: its requirement is its own `alignof`.

A misaligned `location` is a program error, and this specification gives it no meaning.

## No synchronization

A volatile access is not atomic and orders nothing except other volatile accesses. Ordinary memory accesses may move across it. It does not make concurrent access from another execution context well defined. Code that needs indivisibility or inter-context ordering uses [`atomics.md`](atomics.md). Omega has no fence operation for ordering volatile accesses against ordinary ones.

## Compile-time evaluation

A volatile access is a promise about real memory, so `comp` evaluation cannot perform one. Reaching either operation during compile-time evaluation is an error (see [`compile-time-evaluation.md`](compile-time-evaluation.md#unsupported-compile-time-operations)).

## Names

Every exposed `core` item is an ambient fallback name (see [`modules-and-imports.md`](modules-and-imports.md#core-ambient-names)). The `_volatile` suffix is deliberate. Under bare names such as `read` and `write`, a program that meant some other `read` but forgot its import would silently get a volatile access instead of a resolution error.

## Rejected designs

- **A `*volatile T` pointer qualifier.** It doubles every pointer form and hides how many accesses an expression performs: `*p |= 1` is a read and a write, which an operation-per-access spelling makes visible.
- **Platform gaps backed by `asm`.** An `asm` block is a full compiler barrier and forces the value through a stack slot on every access. Volatility is a promise the compiler makes about its own code generation, not a capability a platform supplies.
- **Methods on a marker type** (`Volatile::read`). This adds a constructible type that serves only as a namespace.
