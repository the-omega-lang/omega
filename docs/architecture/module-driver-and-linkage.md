# Driver, module discovery, and linkage

This document is implementation architecture. Source-level module/import semantics live in [`../language/modules-and-imports.md`](../language/modules-and-imports.md); CLI usage lives in [`../guide/compiler-cli.md`](../guide/compiler-cli.md).

## Orchestration boundary

`omgc` constructs `omega_driver::Driver` from the local package root, external roots, target/options, then calls `Driver::compile`. The driver owns whole-package orchestration; the parser is the first source-transformation stage, not the compilation entry point.

The high-level control flow is:

```text
omgc
  -> Driver / ModuleRoots
  -> discover local + external module inventories
  -> parse + macro-expand + HIR lower required modules
  -> collect/resolve/analyze items
  -> CompiledProgram / checked modules
  -> omega-mir
  -> omega-codegen backend
```

## Module roots and discovery

`ModuleRoots` records:

- the local package's declared identity and physical root;
- every `--extern` root keyed by its declared identity;
- eagerly discovered module-path inventories used for deterministic lookup.

The local root is recursively inventoried and every local module is part of the local build set. External roots are inventoried so the driver can answer whether module paths exist and preserve aliases/declared identities without repeated live filesystem guessing.

`core` receives additional eager treatment because primitive/conformance registration and ambient/prelude lookup require its declaration surface independently of ordinary import demand.

Filesystem discovery should remain metadata-oriented: discovering that a module path exists is separate from parsing/resolving all of its contents.

A same-name-directory discovery edge case remains tracked in [`../issues/known-issues.md`](../issues/known-issues.md).

## Parse/index/resolve state

Driver state is grouped by concern rather than one flat global map: roots/discovery, parsed/indexed modules, resolution/import state, primitive/conformance surfaces, diagnostics, and compilation results.

The item-resolution model is deliberately lazy at item granularity where needed (especially generics/imported declarations), while local module membership is eager. Do not conflate “module belongs to package” with “every body is eagerly semantically instantiated.”

## Imports

The driver maps an import alias to a concrete declaration/module target, preserving external/local identity. Resolution must distinguish ordinary imported items, generic item templates, modules, macros, and spec/conformance identities rather than rebuilding paths as if every import were local.

Ambient `core` resolution is a fallback layered on normal local/import lookup.

## Generic instantiations across packages

Generic templates may be declared in an external package and instantiated by a local consumer. The analyzer/driver must retain those instantiations for emission even though the template's source module is not part of the local package's module list.

Separate `omgc` processes may independently generate byte-identical instantiations. Linkage permits the final linker to coalesce the duplicates; this is not a cross-process compiler cache.

## Determinism

Caches that are iterated as ordered declaration/result sets must preserve deterministic order. The current implementation uses insertion-order-preserving maps at sites where iteration has observable consequences (diagnostic order, synthetic IDs, emitted declaration order, tie-breaking).

Do not introduce process-random hash iteration into an output-affecting sweep.

## Symbols and linkage

Omega symbol identity is computed in shared compiler layers above individual backends. The mangling input includes the declaration's module/item identity and generic instantiation identity as required.

Linkage choices support separately compiled packages and duplicate generic instantiations. Backend implementations must consume the already-decided symbol/linkage contract rather than invent their own naming policy.

`@mangling(...)`, root `main`, `gap`/`glue`, and extern declarations create explicit exceptions/bridges; their source-level rules are specified under `docs/language/`.

## Backend seam

The driver does not encode backend-specific semantic acceptance. Once semantic analysis/MIR accepts a program, backend selection should not change the language accepted program set except for an explicitly unsupported target/backend capability reported before emission.

See [`mir-and-codegen.md`](mir-and-codegen.md) for the backend interface and verifier.
