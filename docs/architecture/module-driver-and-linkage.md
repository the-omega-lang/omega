# Driver, modules, queries, and whole-program semantic orchestration

`omega-driver` is the long-lived semantic/package coordinator. It owns the filesystem/module/query lifetime that `omega-analyzer` deliberately does not.

The central architecture is:

> **Every named top-level item is an independent memoized query.**

Same-module references, cross-module references, and concrete generic instantiations all converge on that model.

This document also explains where package identity and emission ownership feed later linkage decisions. The encoding grammar itself is documented in [`symbol-mangling.md`](symbol-mangling.md).

## Entry point

`omgc` constructs a `Driver` with:

- local package root;
- optional declared-name override;
- registered `--extern` roots;
- compilation target.

It then calls:

```text
Driver::compile(entry_module, target) -> CompiledProgram
```

`Driver::compile` is the main semantic-compilation entry point.

## `Driver` state

`Driver` deliberately groups long-lived state by concern:

```text
ModuleRoots     filesystem inventory + package identities
ModuleStore     source / AST / HIR / module index caches
Diagnostics     accumulated structured findings
ItemQueries     memoized signatures, type cells, bodies, instantiations
ImportState     lazy import-alias resolution + usage
Primitives      primitive declaration/templates
Conformances    concrete + generic conformance registrations
prelude_macros  cached ambient exposed core macros
Target          target used by semantic/layout questions
```

The analyzer borrows this state only through `ModuleResolver` queries or focused driver-owned orchestration calls.

## Filesystem discovery and module identity

`roots.rs` + `fs_resolve.rs` are the only layer where an absolute Omega module path becomes a filesystem lookup.

At `Driver::new` time:

- the entire local package tree is discovered into metadata;
- every registered extern tree is also discovered into metadata;
- declared identity overrides relabel module paths in that inventory;
- duplicate package identities are rejected.

After construction, `ModuleRoots::locate` is a map lookup rather than a live filesystem traversal.

This separation matters:

- **existence/discovery is eager metadata**;
- **parsing/semantic resolution is demand-sensitive**.

### Local vs extern eagerness

The local package is the compilation unit being emitted, so every discovered local module is parsed and participates in the compilation even if no sibling imports it.

Extern packages are separate compilation units. Their discovered inventories are available for resolution, but bodies are not compiled as if they belonged to the local package. Required declaration/signature surfaces are collected, and ordinary referenced content is resolved on demand.

## `ModuleStore`

`ModuleStore` keeps every module artifact needed beyond one parse call:

- retained `SourceFile` for diagnostics;
- raw unexpanded `SourceModule` AST;
- module-local raw macro definitions;
- shared macro `ExpansionState` provenance;
- expanded/lowered `HirModule`;
- lazily built `ModuleIndex`;
- structured load failures;
- next real `ModuleId`.

### Parse once

`ensure_ast` reads and parses a physical source file once.

`parse_module` is the memoized module-level pipeline:

```text
locate module
  -> obtain raw AST (cached)
  -> construct visible macro environment
  -> expand macros with provenance
  -> lower expanded AST to HIR
  -> cache ParsedModule
```

A namespace-only directory module has a valid empty `HirModule` and no own source file.

### Index once

`ensure_module_indexed` creates a module's:

- top-level name -> item position map;
- overload-group indexes;
- import alias table.

The local item index is published before imports are fully indexed, because resolving annotations/import metadata may re-enter lookup for the same module. This ordering prevents infinite recursive indexing.

## Import resolution

Imports are stored structurally first and resolved lazily where possible.

`ImportState` memoizes what an alias means and whether it was used. The analyzer can ask the driver for:

- resolved import target;
- raw absolute import path (important for overload resolution);
- candidate import alias names for diagnostics;
- visibility facts;
- ambient `core` fallback candidates.

`core` is special only in well-defined places: exposed ambient names/macros and primitive ownership. Ordinary extern packages do not receive ambient lookup.

## Item query identity

The primary item-query key is:

```rust
ItemKey {
    module: ModulePath,
    name: Ident,
    type_args: Vec<ResolvedType>,
}
```

An ordinary item has empty `type_args`. A concrete generic instantiation is the same query shape with concrete type arguments.

This means there is no second parallel “generic instantiation engine” for named items. It participates in the same caching/cycle machinery.

## Query states and cycles

An item query has a white/gray/black-style state:

```text
absent      -> not started
InProgress  -> signature currently being collected
Done        -> memoized resolved signature available
```

If resolution requests an item already `InProgress`, the driver decides whether the reference is safely indirect or forms an illegal by-value recursive type cycle.

`ModuleResolver::resolve_item` receives an `indirect` flag specifically for this reason: pointers/function type positions can refer through an in-progress type identity without embedding its unfinished layout, while by-value fields/array elements cannot.

## Shared type cells

Struct/enum/union identities use `Rc<RefCell<Resolved*Type>>` cells stored in `TypeCells`.

A cell can be created before all fields/methods/layout facts are populated. This allows recursive **indirect** references to point at stable nominal identity while the signature is still being built.

The cell is then filled by the owning signature analysis and reused everywhere. Downstream references do not copy/reconstruct an aggregate definition independently.

The caches that have output-affecting iteration use deterministic insertion-order maps.

## Two-phase local compilation

`Driver::compile` performs a deliberate signature/body split.

High-level order:

```text
1. discover/parse all local modules
2. collect relevant extern signature surface
3. collect primitive declarations/templates
4. collect conformance declarations/templates
5. collect local item signatures
6. collect gap/glue signature information
7. check local bodies
8. materialize bodies discovered lazily (generic/conformance/primitive)
9. drain diagnostics/warnings
10. run whole-program/local-package analyses such as dead-code/gap checks
11. collect extern-function references
12. return CompiledProgram
```

The exact helper ordering in `compile.rs` is the executable source of truth, but the architectural split is stable: **resolve declarations before body checking consumes them**.

Generic templates themselves have no emitted generic body. Their concrete instantiations are discovered from use sites and checked on demand.

## Analyzer lifetime

The driver constructs a fresh `omega_analyzer::analysis::Analyzer` for focused work such as one item signature or body. The analyzer is then consumed through `finish`, and its errors/warnings/compile-time field-usage information are folded back into driver-owned diagnostics.

This prevents module/query caches from leaking into analyzer-local scope state and prevents one analyzer instance from silently accumulating state across unrelated items.

## The `ModuleResolver` seam

`omega-analyzer::resolver::ModuleResolver` is the explicit dependency-inversion boundary.

The analyzer asks for semantic/module facts such as:

- resolve named item;
- resolve import alias;
- lookup overload candidates;
- obtain raw generic signatures for inference;
- obtain canonical spec declarations;
- prove/query conformances;
- resolve primitive methods;
- request another function body for compile-time execution;
- fetch a previously resolved `comp` value;
- mint a synthetic ID.

The analyzer never calls the filesystem and never owns cross-module query state.

## Generic inference vs instantiation

For argument/field-driven inference, the analyzer sometimes needs the **raw declared shape** of a generic function/type before deciding concrete type arguments. `ModuleResolver` exposes focused raw-signature queries that do not instantiate the item just to inspect its generic pattern.

Once concrete arguments are known, the ordinary `ItemKey` path resolves/checks that instantiation.

Concrete instantiations declared in extern packages may still be emitted by the local compilation that uses them. They are merged into an emitted local `CheckedModule` after semantic phases finish, because this compilation is responsible for producing that concrete body.

## Specs, conformances, and primitives

Specs have a canonical args-independent declaration cell because their raw member/dependency declarations are substituted later against a concrete use.

Conformance and primitive declarations sit beside the ordinary named-item query model because they are relationship/declaration blocks rather than ordinary top-level names.

The driver owns:

- registration;
- template matching;
- goal-directed conformance lookup;
- concrete/lazy template instantiation;
- collecting bodies that become reachable through these relationships.

The analyzer owns the semantic rules for whether a particular declaration/method set satisfies the requested spec/primitive behavior.

## Gaps and glue

Gap declarations define static capability symbol surfaces. Glue declarations implement one gap.

The driver collects/compares these at package scope because uniqueness/completeness is not a local expression-level property. MIR/codegen later receive ordinary function definitions/declarations with the final shared symbol identity; there is no runtime registry.

## Diagnostics and finalization

Errors can arise while queries are triggered indirectly. They are accumulated structurally rather than forcing every lookup caller to own final presentation.

At the end of semantic compilation the driver:

- drains errors for the relevant local + extern signature scope;
- drains local warnings;
- merges field usage from checked and compile-time-evaluated trees;
- performs dead-code and gap sweeps;
- returns errors or `CompiledProgram`.

## `CompiledProgram`

The semantic output contains:

```text
modules            checked modules this compilation will emit
entry              declared entry-module path
warnings           module-tagged analysis warnings
extern_functions   extern-owned function references needed by codegen
```

MIR lowering consumes the checked modules only after this whole semantic stage has succeeded.

## Determinism

Deterministic ordering is a compiler architecture requirement because query discovery order can feed:

- synthetic IDs;
- emitted item order;
- diagnostic order;
- tie-breaking;
- symbol/linkage sets.

Do not replace an ordered cache with randomized iteration where traversal order can escape into output.

## Linkage ownership

The driver decides **which concrete items this compilation owns/emits** and supplies provenance such as generic/conformance origin. It does not encode final object-backend linkage itself.

During checked -> MIR lowering, that ownership/provenance becomes:

- final linker symbol;
- `MirLinkage::Export` or `MirLinkage::Weak`.

See [`symbol-mangling.md`](symbol-mangling.md).

## Maintainer invariants that are easy to break

The driver relies on a few ordering and memoization rules that are not obvious from the public query shape:

- A module publishes its own local item index before import indexing begins. Import annotation resolution can re-enter lookup for that same module; publishing first turns that re-entry into a cache hit instead of unbounded recursion.
- Concrete generic item keys are built only after omitted generic arguments have been filled with defaults. Equivalent call sites must therefore converge on the same structural `ItemKey` and share one instantiation.
- A lazily discovered generic body is checked only after its signature has reached the completed query state. This keeps recursive calls from observing an unfinished body/signature state that the static whole-package sweep could not have enumerated.
- Conformance-template solving is goal-directed and guarded by an in-flight goal stack. Only a failure of an outermost goal is safe to memoize permanently; a nested failure may become applicable when the enclosing proof unwinds and is retried from a clean stack.
- A conformance sweep is not complete if a candidate was skipped because its goal was already in flight. Such a sweep must be eligible to run again later rather than publishing a false-complete memo.
- Explicit/otherwise-higher-precedence conformances are selected before an overridden blanket/template body is analyzed. Diagnostics must not leak from a conformance body that can never be selected or emitted.
- Local and extern modules have different emission ownership. A concrete generic instantiation whose template lives in an extern package is still materialized by the local compilation that requested it; it must not be dropped merely because its template module is absent from the local-module output map.

These are implementation invariants, not language semantics. If the query/conformance architecture changes, update this section with the new invariant rather than recreating long explanatory comments throughout the driver.
