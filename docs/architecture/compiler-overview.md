# Compiler overview

This document is the deep overview of Omega's compiler architecture. It explains the control flow, representation flow, crate boundaries, and where compilation-wide decisions live.

For language semantics, use [`../language/`](../language/). For a compact task router, use the root [`../../ARCHITECTURE.md`](../../ARCHITECTURE.md).

## Two different pipeline views

Omega has a **control/orchestration flow** and a **representation/data flow**. They overlap, but they are not the same diagram.

### Control / orchestration flow

`omgc` is the process entry point. It parses CLI options, builds a `omega_driver::Driver`, asks the driver to perform package discovery + semantic compilation, then explicitly hands the result through MIR and code generation.

```text
omgc
  |
  | Driver::new(package roots, target)
  v
omega-driver
  |  discovers roots/modules
  |  parses/lowers modules through parser + HIR
  |  drives per-item analyzer queries
  v
CompiledProgram
  |
  | omega_mir::lower_program
  v
Vec<MirModule>
  |
  | omega_codegen::generate(CodegenRequest)
  v
object bytes / LLVM IR / assembly
```

The **driver** is therefore the semantic/package compiler orchestrator. `omgc` is the outer toolchain orchestrator that connects the driver to MIR/codegen and owns CLI/output handling.

### Representation / data flow

If the question is instead “what form does source code take as it moves through the compiler?”, the path is:

```text
source text
   |
   v
Token stream
   |
   v
parser AST
   |
   | macro expansion + reparse
   v
expanded AST
   |
   | omega_hir::lower_module
   v
HIR
   |
   | per-item semantic analysis coordinated by omega-driver
   v
checked tree / ResolvedType graph
   |
   | omega_mir::lower_program
   v
MIR CFG + tree-shaped computations
   |
   | shared preflight/ABI + LLVM emission
   v
native object / IR / assembly
```

The parser is the first source transformation stage, but compilation as a whole does not “start in the parser”; the driver/CLI decide when and why a module is parsed.

## Workspace dependency shape

The compiler workspace is deliberately layered:

```text
omega-diagnostics
      ^
      |
omega-parser
      ^
      |
omega-hir
      ^
      |
omega-analyzer <----- omega-driver
      ^                    |
      |                    |
omega-mir                  |
      ^                    |
      |                    |
omega-codegen              |
      ^                    |
      +-------- omgc ------+

omega-mangle  <-- used by omega-mir to encode final linker symbols
```

The important architectural boundaries are:

- `omega-diagnostics` has no Omega semantic knowledge.
- `omega-parser` knows source syntax, not name/type semantics.
- `omega-hir` owns post-expansion identity and syntax-only desugaring.
- `omega-analyzer` knows semantic rules but not filesystems or package caches.
- `omega-driver` owns filesystem/module/query lifetime and implements the analyzer's external resolver interface.
- `omega-mir` owns backend-independent control-flow lowering and final symbol/linkage decisions.
- `omega-codegen` owns native emission through LLVM; it consumes shared semantic/layout/ABI/symbol decisions rather than deriving them.
- `omega-mangle` is intentionally standalone from compiler representations.

## Major representations

### Parser AST

Owned by `omega-parser`. It is close to written syntax and may contain macro definitions/invocations. It has spans and macro token origins, but no durable `HirId` identity.

### HIR

Owned by `omega-hir`. It is the first representation after macro expansion and the first representation with durable source-node identity (`HirId`). It remains mostly syntax-shaped and unresolved.

### Resolved/checked representation

Owned by `omega-analyzer` and assembled by `omega-driver`.

Two related structures matter:

- `ResolvedType` and its shared aggregate/spec cells carry canonical semantic type facts used across many checked nodes.
- `CheckedModule`/`CheckedItem`/`CheckedExprNode` carry the typed, validated tree that later lowering consumes.

Generic templates are not emitted as erased bodies. Concrete instantiations are semantically analyzed and become concrete checked items.

### MIR

Owned by `omega-mir`. Item structure remains close to the checked representation, but each function body becomes an explicit basic-block CFG. Source-level control-flow constructs disappear into blocks and terminators; ordinary computations remain expression trees.

Final function symbols and strong/weak linkage are attached here so codegen always receives the same decisions.

### Codegen state

`omega-codegen` does not define another public IR. It maps MIR into LLVM's native representation: LLVM values/blocks/functions/module/target machine.

Codegen-local caches map Omega/MIR identities to LLVM objects, but codegen is not allowed to redefine language semantics, aggregate layout, linker identity, or the shared Omega calling convention.

## Where compilation-wide state lives

Long-lived compilation state lives primarily in `omega-driver::Driver`:

- package roots and discovered filesystem inventory;
- parsed source/AST/HIR caches;
- module indexes/import state;
- memoized item/spec/overload queries;
- primitive/conformance registrations;
- diagnostics accumulated across throwaway analyzers;
- generic instantiations discovered transitively.

An `omega_analyzer::analysis::Analyzer` is intentionally short-lived: one top-level signature/body (or equivalent focused semantic operation), then discarded. This is a key separation: semantic algorithms live in `omega-analyzer`; cross-module lifetime/caching lives in `omega-driver`.

## The semantic compilation boundary

`Driver::compile` returns `CompiledProgram`, containing:

- local checked modules (plus concrete instantiations that this compilation must emit);
- the package entry module path;
- non-fatal warnings;
- references to extern-owned functions needed by codegen.

By that point:

- local source has been parsed/expanded/lowered;
- required signatures and bodies have been checked;
- generic instantiations used by the compilation have been materialized;
- relevant primitive/conformance/gap/glue relationships have been settled;
- semantic errors have been accumulated and rejected.

MIR lowering and codegen must not re-run semantic resolution.

## Rejectable-input boundary

Most program-invalidity belongs before codegen. `omega-codegen::generate` runs a shared `preflight` pass for the small set of currently unsupported constructs, then checks LLVM target support.

After that point, a codegen failure should normally mean one of:

- unsupported target/ISA for this LLVM build;
- explicit symbol collision caused by mangling controls;
- object/IR construction failure;
- an internal compiler bug (for example LLVM verifier failure).

A new language validity rule should not be implemented independently inside codegen.

## Target propagation

`omega_analyzer::Target` is the compiler-wide target vocabulary. The same target is supplied to semantic compilation and codegen so pointer-width-dependent semantic/layout questions agree with emission.

LLVM-specific target triples/settings are derived only inside `omega-codegen::llvm`.

## Separate compilation

Omega packages are compiled in independent `omgc` processes and later linked as normal object files. `--import` registers dependency roots for semantic resolution; it does not merge packages into one source compilation unit.

This makes several facts cross-process contracts:

- declared package/module identity;
- symbol mangling;
- weak/strong linkage policy for independently regenerable monomorphizations;
- shared Omega ABI;
- externally visible aggregate representation where part of the ABI.

The repository's integration recipes deliberately link objects produced by different compiler invocations to exercise these contracts.

## Runtime/library position

`runtime/core`, `runtime/std`, and a selected `runtime/plat/*` directory are ordinary Omega packages compiled into separate objects. The compiler does not silently inject a native runtime object.

`core` has limited language/tooling privilege: ambient exposed-name/macro lookup and primitive declaration ownership. `std` and `plat` are ordinary explicitly registered packages. Platform capabilities are connected through Omega's `gap`/`glue` mechanism rather than an implicit runtime registry.

See [`runtime-and-platform.md`](runtime-and-platform.md).

## Architectural extension points

### Add syntax without new semantics

Parser AST -> HIR lowering only. Keep analyzer/codegen closed unless the syntax changes the semantic representation.

### Add a semantic feature

Usually HIR (if a new source shape is needed) -> analyzer/driver -> checked representation. MIR/codegen only need changes if the feature changes runtime control flow or representation.

### Add a target

Extend the shared `Target`, then deliberately audit pointer width/layout, shared ABI assumptions, and LLVM target support. A target being expressible in the shared vocabulary does not imply LLVM supports it.

### Add a package/runtime capability

Prefer ordinary Omega packages and `gap`/`glue` over compiler magic. Compiler support is warranted only when the capability is genuinely a language/ABI primitive.
