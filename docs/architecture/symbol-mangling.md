# Symbol mangling and linkage

Omega separates **symbol grammar** from **compiler-to-symbol adaptation**.

```text
checked semantic item
   |
   | omega-mir::mangle builds standalone symbol model
   v
omega_mangle::Symbol
   |
   | omega_mangle::encode
   v
final linker string
   |
   + MirLinkage decided during MIR lowering
   v
all backends consume same symbol + linkage
```

## `omega-mangle` is standalone

The mangling grammar keeps encoder and decoder tag definitions centralized in
`omega-mangle::grammar`. Backreferences are byte offsets to previously completed
path/type substitutions in already-emitted mangled text. The decoder resolves
only completed substitutions and rejects forward, self, cyclic, or otherwise
unresolved references.

The `omega-mangle` crate intentionally does not depend on `omega-analyzer`, HIR, MIR, or a backend.

It owns:

- `symbol.rs` — generic symbol/path/type model;
- `grammar.rs` — tag grammar, including namespace discriminators;
- `encode.rs` — encoding + backreference compression;
- `decode.rs` — grammar decoding + validation;
- `display.rs` — human-readable rendering of the decoded model;
- `base62.rs` — compact integer grammar;
- `omg-demangle` — small CLI tool.

This makes the linker-name grammar testable/toolable without embedding compiler-internal type graphs.

## Symbol model

A `Symbol` contains:

```text
path
optional function signature
optional vendor suffix
```

`ManglePath` can represent:

- package/module root;
- nested type/value namespaces;
- generic application;
- structural-type owner (needed for conform methods on unnamed targets such as primitives/slices).

`MangleType` mirrors externally identity-relevant semantic type shapes without depending on `ResolvedType` directly.

Nominal types are encoded by path + generic args rather than recursively encoding their fields, which keeps recursive nominal types finite.

## Overloads

Unlike Rust v0 mangling, Omega's function signature is load-bearing because Omega supports function overloading. Parameter + return type identity participates in the symbol so overloads with the same source path remain distinct.

Method `self` shape is represented through the actual leading receiver type in the signature rather than a separate mangling tag.

## Compiler adapter: `omega-mir::mangle`

`omega-mir::mangle` translates compiler semantic data into the standalone mangling model and then calls `omega_mangle::encode`.

It owns construction helpers for:

- free function symbols;
- globals;
- methods;
- gap/glue functions;
- primitive methods;
- conformance methods;
- vtables;
- extern references;
- content-addressed data symbols.

Because this happens during checked -> MIR lowering, both backends receive only the finished name.

## Package/module identity

The first mangled path segment is the **declared package root identity**, not an arbitrary filesystem path. `--name` / `--extern=<name>:<dir>` therefore affect source-visible/module ABI identity consistently.

Nested modules and item owners extend that path deterministically.

Separately compiled packages must be invoked with identities that agree with the source imports and symbols they expect to link.

## Generic identity

Concrete type arguments participate in a generic item's mangled path. A monomorphized `Foo<u32>` therefore has stable identity distinct from `Foo<u64>`.

Generic function/method instantiations similarly include concrete args as needed in the path/signature model.

## Conformance method identity

A conformance method's identity must include enough information to distinguish:

- target type;
- spec identity;
- spec generic arguments;
- method/function signature.

Unnamed target types cannot be represented as normal type paths, so `ManglePath::Type(MangleType)` provides a structural owner path.

The current implementation still omits the spec's *module path* in conformance-method and vtable identity; that ABI bug is tracked explicitly in [`../issues/known-issues.md`](../issues/known-issues.md) and must be migrated as one cross-package mangling change.

## Gap/glue identity

A gap function and its matching glue function intentionally map to the same final symbol. The declaring package can therefore emit an extern reference while the selected platform/final program emits the definition, with no registry or trampoline.

## `main`

The allowed root entry `main` receives a fixed internal symbol, `_omg_main`, instead of ordinary mangling -- forced rather than derived because ordinary mangling encodes the return type, and `main`'s two allowed return types (`void`, `never`) would otherwise mangle to different strings for what must be one stable linkage contract. It is deliberately not the platform's native entry-point symbol (e.g. C's `main`); a `plat` implementation that wants to produce a runnable native program supplies its own adapter under the real native entry symbol and calls `_omg_main` (see `docs/language/foreign-function-interface.md`, "Program entry point"). A child-module function also named `main` remains normally mangled.

This exception is decided before backend emission so both backends agree.

## Mangling controls

Resolved `@mangling(...)` metadata travels from semantic analysis to checked/MIR items. The MIR adapter applies the final enabled/disabled/forced symbol policy.

A forced/disabled policy can create a real duplicate linker name. Codegen maintains a symbol-collision guard and reports such collisions rather than allowing backend/library behavior to choose a winner silently.

## Linkage

`MirLinkage` currently distinguishes:

```text
Export   strong definition
Weak     independently regenerable definition; duplicates may be folded
```

Weak linkage is used when separate compilations can legitimately generate byte-equivalent definitions under the same symbol, especially concrete generic/template instantiations and monomorphized conform methods.

A hand-written concrete declaration that should exist exactly once remains strong so a duplicate is diagnosed by the link model rather than silently folded.

The driver provides the ownership/provenance facts; MIR lowering converts them to final linkage.

## Extern-owned functions

The semantic driver collects references to non-generic extern-owned functions needed by the local program. Codegen declares their already-computed symbols without compiling their bodies.

A concrete instantiation of a generic template declared in an extern package is different: the local compilation that needs the concrete instantiation may be responsible for emitting it, using weak linkage so another package independently producing the same instantiation can coexist.

## Vtables

Dynamic spec coercion produces a resolved ordered method-slot list. Backends may deduplicate vtable data by the resolved slot list within one compilation unit.

External symbol identity for a vtable cannot rely on local `HirId`s, because IDs have no cross-process meaning. `mangle::vtable_symbol` derives a deterministic symbol from semantic type/spec identity instead.

## Anonymous data symbols

Compiler-generated constant blobs use content-addressed symbols so identical data receives the same weak identity across modules, separate compilations, and backends. The hash is deliberately non-cryptographic; the threat model is accidental collision among compiler-produced constants, not adversarial input.

The hash input is the constant's **logical canonical content**, not merely the raw bytes of the eventual object buffer. Pointer-bearing constants can contain zero placeholders in the physical bytes while their actual targets live in relocations; hashing only those bytes could collapse distinct constants onto one weak symbol and silently select the wrong data. Canonical serialization therefore includes the pointed-to logical content (with explicit length where needed) before deriving the symbol.

Content hashing/deduplication is an emission implementation detail distinct from source item mangling, but it follows the same requirement that a repeated compiler-generated identity be deterministic.

## Mangling changes checklist

A mangling change is an ABI/separate-compilation change. Audit:

1. encoder + decoder/demangler round-trip;
2. `MangleType` coverage for all external type identities;
3. MIR adapter construction for free/method/conformance/primitive/gap/vtable cases;
4. overload uniqueness;
5. package `--name`/extern identities;
6. weak/strong duplicate behavior;
7. cross-process and mixed-backend linking tests.

Do not update one backend's symbol naming as the implementation of a mangling change.
