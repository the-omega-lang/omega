# Mangling annotations on global data

## Task Description

- **Deliverable:** Allow `@mangling(enabled)`, `@mangling(disabled)`, and `@mangling(force = "...")` on ordinary module-level storage bindings. Preserve and test the existing support on foreign data bindings. Both requested examples must compile and link when their external dependencies are supplied:

  ```omega
  @mangling(force = "symbol_from_outside")
  foreign outside_sym : i32;

  @mangling(force = "unmangled_symbol_with_default_value")
  my_symbol : i32 = 10;
  ```

- **Purpose:** Give Omega globals explicit linker names for native interoperability and separately compiled objects, without allocation, runtime adapters, or a libc dependency.
- **Chosen direction:** Extend the existing annotation pipeline. The analyzer resolves policy once; the driver preserves it; MIR assigns the final name; codegen consumes that name through its existing declaration catalog and collision guard.
- **Rejected alternatives:** No new export/rename annotation, backend-specific name selection, compatibility aliases, or relaxation of symbol-collision rules. Do not represent ordinary data as foreign bindings: a definition still owns its storage and initializer.

## Technical Details

**Initial context boundary:** Start with `docs/language/annotations-and-sizeof.md`, `foreign-function-interface.md`, and `bindings-and-mutability.md`, plus `docs/architecture/symbol-mangling.md`. The implementation crosses parser → HIR → analyzer → driver → MIR because ordinary globals currently have no annotation channel. Codegen needs verification of the existing contract, not a new naming policy. Consult `docs/guide/quick-reference.md` for Omega fixtures and `docs/architecture/testing-and-validation.md` for their execution.

**Settled semantics:**

- Support typed globals with or without initializers and inferred `:=` globals, both immutable and `mut`, under all existing visibility modes. Ordinary globals default to `enabled`; foreign bindings continue to default to `disabled`.
- `enabled` keeps the current module-qualified global symbol byte-for-byte; `disabled` uses the source identifier; `force` uses the existing non-empty string policy. Source lookup, visibility, storage ownership, mutability, layout, alignment, and initialization rules do not change.
- A stored value uses global/data symbol construction even if its type is a function type. Preserve the distinct existing interpretation of a function-typed **foreign** binding as an external function symbol.
- `comp` bindings have no runtime symbol and remain ineligible. Locals, parameters, fields, aliases, types, and other currently unsupported targets do not gain mangling support. Existing function/method restrictions remain intact.
- Adding annotation syntax to globals must not accidentally enable other annotations there, including `@suppress`. Unknown names, duplicates, malformed arguments, and unsupported annotations must produce explicit diagnostics.
- Distinct items with the same final symbol remain errors within one compilation, including global/global, global/function, and global/foreign-data collisions. A foreign binding and an ordinary definition with the same name are not a new same-compilation alias mechanism. Existing gap/glue behavior is unchanged.

**Verified implementation sites and decisions:**

| Boundary | Sites and required change |
|---|---|
| AST/parser | `compiler/omega-parser/src/ast/item.rs`: add annotations to the three ordinary global `Item` variants by converting them to named-field variants. Keep shared local/field `DeclarationStmt` and `WalrusStmt` types free of global annotation state. `src/parser/item.rs::parse_item` and `src/parser/item/functions.rs` currently reject annotations on ordinary globals; carry them through typed/inferred and modifier paths, retaining rejection for `comp`. |
| Macro expansion/HIR | Update the affected matches/reconstruction in `compiler/omega-parser/src/macros/expander.rs` so initializer expansion preserves annotations. Add annotation fields to the corresponding three `HirItem` variants in `compiler/omega-hir/src/hir.rs`; lower them with the existing `lower_annotations` helper in `src/lower/item.rs`. Update other affected variant matches mechanically. |
| Annotation analysis | `compiler/omega-analyzer/src/annotations.rs`: add an ordinary-global `ItemKind` and its diagnostic names; allow it only for `mangling`. Reuse `resolve_mangling` and the existing duplicate/argument diagnostics. The current `suppress` arm has no target guard, so explicitly reject it on this new kind without broadening this task into an annotation-system rewrite. |
| Checked globals | `compiler/omega-analyzer/src/checked.rs::CheckedDeclaration` gains resolved mangling policy. `src/analysis/items/mod.rs` owns `analyze_declaration`, `analyze_global_declaration_with_init`, `analyze_global_walrus`, and `finish_global_binding`: resolve global annotations at these entry boundaries with default `Enabled`. Use a global-specific entry for uninitialized declarations; keep local declaration analysis annotation-free. Update local/synthetic `CheckedDeclaration` constructors with the inert `Enabled` default. |
| Driver persistence | `compiler/omega-driver/src/items/resolution.rs` currently discards checked global metadata, caching only initializers. Replace `ItemQueries::global_initial_values` in `src/items/mod.rs` with a `HirId`-keyed cache of complete checked global declarations. Cache all three ordinary storage forms after successful analysis. `src/bodies.rs::check_item_body` must clone those checked declarations instead of reconstructing them and losing policy. Keep `comp_values`, function annotations, and `ResolvedItem::Value` unchanged. |
| Final symbols | `compiler/omega-mir/src/lower/item.rs::lower_declaration` currently always calls `mangle::global_symbol_string`. Apply enabled/disabled/forced policy there, consuming `CheckedDeclaration.mangling`; `Glued` is unreachable for ordinary globals. `MirDeclaration.symbol` already carries the final result, so no new MIR metadata or `omega-mangle` grammar is needed. Preserve `foreign_binding_symbol` behavior. |
| Emission/collisions | `compiler/omega-codegen/src/catalog.rs::collect_item` already registers ordinary and foreign data symbols; `src/symbol.rs::SymbolRegistry` shares the namespace with functions. Preserve these mechanisms and prove that cross-source declarations use the selected name. |

**Interfaces/invariants:** HIR stores syntax, not resolved policy. Signature analysis validates annotations even for unused local-package globals; body materialization does not resolve them again. Cache retrieval for a successfully resolved global is an invariant, not a reason to silently substitute default mangling. Declaration identities remain unchanged, so references still target the same storage. Preserve existing unannotated symbol strings and strong definition behavior.

**Out of scope:** The documented “Reading an extern package's data global crashes the compiler” issue in `docs/issues/known-issues.md` needs a separate extern-global reference channel. Do not repair it here or claim this feature fixes direct imports of externally owned globals. Cross-process tests below use explicit foreign bindings in the consumer. Also exclude mangling grammar changes, runtime changes, new initializer capabilities, and general foreign/function collision cleanup.

**Risks/open questions:** No unresolved language-design decision is required for this scope. If implementation reveals that explicit foreign data references require the separate extern-global import redesign, stop and report that dependency rather than introducing a backend workaround or silently expanding scope.

## Implementation Plan

1. Extend the three AST/HIR global variants and their parser, macro-expansion, and lowering paths. Preserve annotation argument spans. Update dependent pattern matches and add focused parser/HIR preservation tests. Keep annotated `comp` and local declarations rejected.
2. Add global annotation resolution and checked policy, then replace the driver's initializer-only cache with checked-global persistence. Cover initialized, uninitialized, and inferred declarations; retain existing declaration/type/initializer checks. Verify rejected annotations are diagnosed once, including on unused globals.
3. Apply policy in MIR global lowering. Add exact-symbol tests alongside `a_global_symbol_is_decided_by_its_declaring_module_before_emission`; retain that existing default-mangling regression. Update checked-declaration test constructors as required. Verify codegen's existing catalog consumes the result across source files and rejects collisions.
4. Update `docs/language/annotations-and-sizeof.md` applicability and naming rules; extend `docs/language/foreign-function-interface.md` symbol-naming defaults/examples to mention ordinary data globals. Add a short binding cross-reference in `docs/language/bindings-and-mutability.md` and the two requested examples under annotations in `docs/guide/quick-reference.md`. Describe the checked-global policy/cache handoff briefly in `docs/architecture/symbol-mangling.md`. Keep the unrelated extern-global issue intact.
5. Add executable positive/negative conformance fixtures and a separate-process linkage regression as specified below. Run focused checks, then the normal full gate. Deliver the source/docs/tests together; do not claim validation that was skipped.

## Testing

**Component coverage:**

- Extend `compiler/omega-parser/tests/refactor_regressions.rs` and `compiler/omega-hir/tests/lowering.rs`: preserve annotations through each global form, `mut`/visibility prefixes, and an expanded initializer or item-producing macro. Preserve existing foreign parsing coverage in `compiler/omega-parser/tests/foreign.rs`.
- Add `compiler/omega-driver/tests/global_mangling.rs` (new), using the package/diagnostic harness pattern in `tests/diagnostics.rs`: assert resolved checked policy and initializer preservation for all three forms, defaults/overrides for foreign data, and typed diagnostic kinds for invalid input. Include an unused invalid global to protect signature-time validation.
- Extend MIR's tests in `compiler/omega-mir/src/lower/item.rs` for exact forced/bare/default names and module-sensitive enabled names. Include ordinary storage of function type at this component boundary so it cannot accidentally take the foreign-function naming branch.
- Extend `compiler/omega-codegen/tests/emission.rs`: the owner defines the requested global symbol and another source declares/references that exact symbol. Assert selected initializer values and absence of a second definition. Check global/global and global/function collisions across source files, plus global/foreign-data collisions, by diagnostic text containing the offending symbol; no LLVM auto-renaming or link-failure-only assertions.

**Language conformance and specification trace:**

- Add `tests/global_mangling/` (new) with Omega sources, freestanding `native.c`, and exact `expected.stdout`/`expected.stderr`. This proves the extended annotation rules and existing FFI data rule through compile/link/run. The C helper defines `symbol_from_outside`; Omega accesses it as `outside_sym`. C reads `unmangled_symbol_with_default_value`, confirming the Omega definition initializes it to `10` under that exact external name.
- In the same focused package, cover `disabled` and inferred/mutable globals, writes through the Omega source name observed by C, and a renamed global referenced from another Omega source. Retain explicit enabled/default controls. C helpers must not require libc, and must only mutate globals whose Omega binding is mutable.
- Add `tests/global_mangling_invalid_args/` and `tests/global_mangling_invalid_target/` (new), each with exact compiler `expected.stderr`. Cover empty forced names, malformed mode/arguments, duplicate annotation, a non-mangling annotation on a global, and `@mangling` on `comp`/local storage. Keep independent cases separate if parser recovery would hide a later diagnostic. These prove the annotation validation/applicability rules; component tests may cover the larger diagnostic matrix.
- Add `compiler/omgc/tests/global_mangling_linkage.rs` (new), following the native-tool harness conventions in `separate_linkage.rs`: invoke `omgc` separately for an Omega producer defining forced/disabled data and an Omega consumer using explicit foreign data bindings under different source names; link and execute via a small C harness. Assert values and matching addresses. Do not place both producer definition and its foreign declaration in one compilation. This proves exact symbol identity across processes without depending on the known extern-global import bug.

**Commands/target coverage:** Start with the new focused Rust tests, then run:

```text
cargo test -p omega-parser -p omega-hir -p omega-analyzer -p omega-driver -p omega-mir
cargo test -p omega-codegen --test emission
cargo test -p omgc --test global_mangling_linkage
just build-omgc build-runtime
./bin/test-runner global_mangling global_mangling_invalid_args global_mangling_invalid_target t15_annotations_and_sizeof t19_foreign_function_interface
just test-all
```

The host LLVM compile/link/run path and freestanding C helpers are sufficient: this adds policy selection without changing target symbol encoding or ABI representation. No unrelated platform matrix is required. If an additional negative package is needed for isolated diagnostics, include it in the focused command. Report any unavailable native tool as a skipped check with its reason.
