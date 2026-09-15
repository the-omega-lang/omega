# Compiler definitions and conditional compilation

## Task Description

- **Deliverable:** `omgc -Dname[=literal]`, immutable compiler definitions, reusable nested annotation-call syntax/evaluation, and top-level `@cond(condition)` filtering before macro binding/expansion and semantic registration.
- **Purpose:** select platform/configuration-specific declarations without runtime branches, generated allocations, extra symbols, or a libc dependency.
- **Chosen direction:** the parser represents syntax; a pure analyzer-owned evaluator interprets conditions against explicit configuration; the driver filters before building macro environments and HIR. A parser-owned expansion hook applies the same evaluator to generated items before recursively expanding them.
- **Rejected alternatives:** C-style token replacement or ordinary global names; arbitrary Omega expressions/functions through const-eval; pruning after semantic registration or in MIR; parser dependence on target/analyzer semantics; duplicate evaluators.
- **Planning assumptions:** include imports, aliases, macro definitions, and top-level macro invocations; check all condition operands without short-circuit suppression of errors. These defaults follow the requested top-level scope and “any reference” rule. The user was asked about both during planning; subsequent feedback supersedes these defaults.

## Technical Details

### Context and verified integration sites

Start with `docs/language/annotations-and-sizeof.md`, `lexical-structure.md`, and `types-and-primitives.md` in that directory, plus `docs/architecture/parsing-and-hir.md` and `module-driver-and-linkage.md`. Read `docs/guide/quick-reference.md` before writing Omega fixtures.

| Existing owner/site | Change |
|---|---|
| `compiler/omgc/src/cli.rs::{Args, parse_compile}`, `app.rs::compile` | Collect definitions, validate using the final target, construct the configured driver, update help. |
| `compiler/omega-parser/src/ast/annotation.rs`, `parser/item/annotations.rs`, `prelude.rs` | Recursive, spanned annotation expressions and standalone literal parsing. |
| `compiler/omega-parser/src/ast/item.rs::ItemNode`, `parser/item.rs::parse_item` | Retain top-level conditions independently of item-specific ordinary annotations. |
| `compiler/omega-parser/src/macros.rs::{expand_with_origins, collect_definitions}`, `macros/expander.rs::{Expander, expand_item_list, expand_items_invocation}` | Filter before macro collection and before recursive expansion of generated items. |
| `compiler/omega-analyzer/src/target.rs::{Target, Arch::name, Os::name}` | Authoritative target builtin values. |
| `compiler/omega-analyzer/src/analysis/literals.rs::parse_number_literal` | Reuse/extract pure decoding where appropriate; the ordinary analyzer entry point requires resolution and cannot run during filtering. |
| `compiler/omega-analyzer/src/annotations.rs` | Keep existing annotation argument contracts strict. |
| `compiler/omega-driver/src/lib.rs::Driver`, `compile/mod.rs::Driver::compile` | Freeze configuration before source loading; prevent cache reuse under a different target/configuration. |
| `compiler/omega-driver/src/modules.rs::{ensure_ast, module_macros, macro_env, parse_module, ModuleStore, LoadFailure}`, `error.rs::CompileError` | Cache filtered AST, integrate the expansion hook, report errors before HIR exists. |
| `compiler/omega-hir/src/hir.rs::{HirAnnotationArg, HirAnnotationValue}`, `lower/item.rs::lower_annotations` | Preserve new generic annotation syntax for ordinary annotations; conditions have already been consumed. |

Add analyzer modules `compiler/omega-analyzer/src/compiler_definitions.rs` and `annotation_eval.rs` (proposed new files). No feature logic belongs in MIR, codegen, runtime, layout, mangling, or ordinary name resolution.

### Definition and CLI contract

1. Accept attached `-Dname`, `-Dname=value`, and separated `-D name[=value]`. No value means boolean `true`; an explicitly empty value is invalid. Split at the first `=` only. Names are case-sensitive single Omega identifiers, including contextual identifiers; reject empty names, hard keywords, and qualified paths.
2. Every repeated name is an error, even with identical values or mixed CLI forms. Preserve both occurrences for diagnostics and input order for deterministic reporting. Never collect directly into an overwriting map.
3. Accept one complete lexical literal: boolean, integer, decimal float, character, string, or byte string, including a leading minus on numeric literals. Reuse Omega bases, underscores, suffixes, escape decoding, and tokenization. Reject extra expressions/tokens, names, macro calls, malformed values, invalid suffixes, and range overflow. Arrays and named aggregate constructions are expressions outside this lexical-literal input; membership lists are separate annotation syntax.
4. Preserve kind and explicit numeric type. Unsuffixed CLI numbers use Omega defaults (`i32`, `f32`); suffixes use the existing primitive set and selected target's `usize`/`isize` widths. Validate all definitions, including unused definitions, against the **final** target, independently of option order. Decode negative integers as sign plus magnitude so signed minima work; negative unsigned values are errors. Reject nonfinite/out-of-range floats and round to the declared width before comparison.
5. Document shell quoting: `omgc src/ -o out/ -Dcount=123 '-Dlabel="release build"' -Denabled`. An unquoted literal name such as `-Dlabel=release` is invalid, not an implicit string. Characters and byte strings retain distinct kinds.
6. `def::name` accesses only user definitions. Bare builtin names cannot be overridden: `-Dtarget_os='"custom"'` affects only `def::target_os`. Neither namespace participates in ordinary expressions, `comp` bindings, imports, aliases, shadowing, or macro name lookup. There is no implicit environment-variable input.
7. One configuration applies to every source read by one invocation, including external packages and ambient `core` macros. Separate invocations must receive compatible definitions wherever shared declarations/ABI depend on them. Do not encode configuration in linker names or implement prebuilt-object compatibility checking.

Use an owned immutable value enum retaining primitive kind/width and owned string/byte contents, with CLI origin records. No checked expressions, HIR IDs, resolver handles, or runtime addresses. Lookup returns present/absent; **the boolean evaluator context**, not the environment, supplies false for absence. This reusable value/lookup boundary prepares for future explicit binding access without implementing it.

### Initial builtins

| Name | Type/value | Source of truth |
|---|---|---|
| `target_os` | string: `none`, `linux`, `macos`, `windows` | `Target.os.name()` |
| `target_arch` | string: `x86_64`, `x86`, `armv7`, `thumbv7em`, `aarch64`, `riscv32`, `riscv64`, `avr` | `Target.arch.name()` |
| `target_pointer_width` | `u32`: 16, 32, 64 | `Target.pointer_bits()` |
| `target_freestanding` | boolean | `Target.os == Os::None` |

Builtins describe the selected target, never the host. CLI aliases normalize (`i686` → `x86`, `darwin` → `macos`, `freestanding` → `none`). Comparison strings remain ordinary strings: do not normalize or validate them against a target whitelist. Thus `in(target_os, &["linux", "openbsd"])` is valid without adding OpenBSD support. Unknown bare names are errors. Defer CPU features, endianness, vendor/ABI families, compiler versions, and optimization predicates until their contracts/shared ownership are established.

### Annotation expressions and evaluation

Add a reusable recursive annotation-expression node with span and token origin: literal, bare identifier, qualified reference, call, and list. Extend positional and key/value arguments to carry these forms while preserving existing annotation contracts. In the condition context, qualified references must have exactly the shape `def::identifier`; other paths do not enter ordinary name resolution. Do not parse arbitrary Omega expressions for later subset evaluation. Call names, signatures, and result kinds belong to the consuming annotation context; a small typed dispatcher is sufficient, without a global function/plugin registry.

`@cond` takes exactly one positional boolean condition. Bare `@cond`, `@cond()`, named/extra arguments, and duplicate `@cond` on one node are errors, regardless of order/truth value. Commas are mandatory separators; the draft multiline `all` example needs its missing comma. Allow an optional trailing comma in new nested annotation calls/lists, without changing ordinary Omega call/array grammar.

| Form | Contract |
|---|---|
| `true`, `false`, boolean builtin, `def::flag` | Boolean condition; supplied nonboolean values error; absent user definitions are false. |
| `not(condition)` | Exactly one boolean operand. |
| `all(condition, ...)`, `any(condition, ...)` | Zero or more boolean operands; empty `all` is true, empty `any` is false. |
| `equals(value, value)` | Exactly two comparable values. Inequality is `not(equals(...))`. |
| `less(value, value)`, `less_equal(value, value)`, `greater(value, value)`, `greater_equal(value, value)` | Exactly two numeric values of the same family. |
| `in(value, &[value, ...])` | Exactly a value and a literal list; each element must be comparable to that value. Empty list is false after validating the first operand. |

All calls return boolean. `&[...]` is an annotation membership-list spelling with no allocation, address, or slice semantics. No alternative list spelling, indexing, nested lists, or list-valued CLI definitions. Boolean-returning calls can also be equality/membership operands.

**Missing definitions are local to operand position.** Only the argument of `@cond`, `not`, `all`, or `any` is expected-boolean. Missing user definitions there are false; supplied numbers/strings are errors without truthiness conversion. Equality/ordering/membership arguments are value positions: every referenced definition must exist, even `equals(def::flag, false)`. Boolean use elsewhere does not establish a global type. No cross-item inference or presence operator.

**Check every operand left to right.** `any(true, equals(def::missing, 123))` and `all(false, bad_call())` error. Once the outer condition is valid and false, discard the item without semantic inspection of its contents/nested conditions. Syntax and parser-enforced placement still must be valid.

**Comparison:** boolean/char/string/byte-string equality is by value within the same kind; compare decoded string/byte contents. Integers compare mathematical values across widths/signedness without wrapping or floating conversion. Floats compare values rounded to their declared width, promoting `f32` exactly to `f64` for comparison; signed zeros are equal. Integer/float and other mixed-kind comparisons error. Unsuffixed source numbers use a counterpart's established numeric type when compatible, otherwise ordinary defaults, symmetrically in either operand order. CLI definitions retain their established type. In membership, the checked first operand supplies context to list literals; list elements do not infer its type. These are annotation-evaluator rules, not changes to ordinary Omega coercions/operators.

### Filtering and configuration invariants

1. Support every top-level `Item`: ordinary/foreign functions and bindings, storage/`comp` bindings, structs/markers/unions/enums/specs, gaps/glues/meets/primitive blocks, imports/aliases, macro definitions/invocations, and whole foreign blocks. Reject individual methods, fields, variants, parameters, foreign-block entries, statements, and expressions. A condition on a container controls the whole container.
2. Add `ItemNode.conditions: Vec<AnnotationNode>` (proposed field). `parse_item` extracts only `cond` before forwarding other annotations through existing item parsers. Preserve the vector to diagnose duplicate spans. Keep existing per-function/type annotation storage; do not relocate all ordinary annotations or add condition fields to every variant.
3. Lex and parse the full physical source first. Disabled source must remain grammatically valid, including parser-enforced annotation placement. Ensure nested placement is rejected even inside otherwise unused generic declarations; the existing nested annotation entry sites are `parser/item/definitions.rs::parse_member_functions` and `parser/item/foreign.rs`. Ordinary semantic annotation checks do not run on removed items.
4. `ensure_ast` evaluates/removes top-level items **before publishing the AST to any consumer**; consume conditions on retained items. Raw macro definitions, aliases, imports, prelude macros, collision checks, and environment registration must all see this same filtered AST. Cache condition failures through the module-load failure path; errors must never become false/successful empty modules.
5. Add a fallible caller-provided item-filter hook to macro expansion, before raw macro collection and at each `expand_item_list` iteration. Invoke it on newly reparsed macro-generated items before recursively expanding their bodies/invocations. Use the same evaluator/configuration as `ensure_ast`. The parser owns traversal only; a generic/wrapped expansion failure distinguishes macro and callback errors without importing analyzer types. Existing syntax-only expansion APIs may use an identity filter.
6. Preserve token origins and macro expansion context in condition diagnostics. Template substitution may produce conditions normally; annotation expressions cannot invoke macros. Retain existing restrictions on active macro-generated macro definitions and existing discovery behavior for generated imports/aliases. False generated items disappear before subsequent operations on them.
7. Only surviving expanded items reach infallible HIR lowering. No disabled-item flags in HIR/checked output. Removed items cannot claim names, add overloads, resolve aliases/imports, warn about imports, register primitives/conformances/glues, undergo signature/body checking, instantiate generics, or reach MIR/native output. Mutually exclusive same-name items are allowed; two enabled conflicting items retain normal diagnostics. Enabled references to removed names fail normally.
8. Keep physical source inventory and per-file output slots: all local files still parse, even those left empty. A condition does not disable package/directory discovery or syntax errors elsewhere.
9. Freeze target/definitions before any caches are populated. Preserve `Driver::new(..., target)` as empty-definition convenience; add a configured constructor (proposed `new_with_definitions`). Remove the redundant target parameter/assignment from `Driver::compile` and migrate callers mechanically. Different configurations require separate drivers. The CLI passes the same selected target to codegen. No mutable configuration setters or configuration-blind reuse.

**Escalation risks:** stop if evaluation requires ordinary semantic queries, any raw macro lookup can bypass filtering, or literal fidelity requires changing general numeric semantics. Extract narrowly reusable pure decoding; do not turn this into a numeric-literal overhaul.

**Out of scope:** runtime/binding access to definitions, token replacement/interpolation, general compile-time calls/operators, nested conditions on members/statements, new targets, platform/runtime migration, package feature resolution, configuration-specific linker symbols, and an annotation plugin framework.

## Implementation Plan

1. **Reusable syntax:** extend parser annotation AST/parsing and exports, standalone literal parsing, HIR annotation forms/lowering, and strict existing annotation validators. Reuse lexer facilities and honor nesting limits/recovery. Preserve existing `layout`, `symbol`, `inline`, `naked`, and `suppress` behavior. Add syntax component tests.
2. **Pure definitions/evaluation:** add the proposed analyzer modules/exports, typed values, target builtins, local expected-kind rules, eager comparisons/call dispatch, and diagnostics with spans/origins but no HIR identity. Reuse/extract numeric decoding only where suitable; test sign/range/floating boundaries explicitly.
3. **Configuration/CLI:** collect `-D` options and update help in `cli.rs`; validate with the final target and pass configuration in `app.rs`. Add the configured driver constructor, eliminate the compile-time target override, and migrate callers by symbol search. Keep empty-definition construction available. Add CLI/configuration tests.
4. **Filtering:** add/extract `ItemNode.conditions`, enforce placement/duplicates, filter in `ensure_ast`, integrate the fallible recursive macro hook and error rendering. Update every `ItemNode` constructor by reference search, preserving conditions until consumed. Prove removal before macro binding and semantic registration before proceeding.
5. **Documentation:** put the normative contract/builtin table in `docs/language/annotations-and-sizeof.md`; update annotation/item productions in `docs/language/grammar.md`, the blanket prohibition in `docs/language/aliases.md`, binding applicability in `docs/language/bindings-and-mutability.md`, and contradictory `docs/guide/quick-reference.md` examples. Document quoting/configuration/separate builds in `docs/guide/compiler-cli.md`. Update the two selected architecture documents and compact `ARCHITECTURE.md` pipeline for early filtering/immutable configuration, without duplicating the specification.
6. **Conformance/review:** extend the runner narrowly for definitions, add the cases below, and run the gates. A fresh reviewer should check phase ordering, skipped semantic effects, macro provenance, numeric fidelity, configuration immutability, and ordinary-annotation compatibility.

## Testing

### Component/CLI cases

- **Parser/HIR:** proposed `compiler/omega-parser/tests/conditional_compilation.rs`: recursive calls/lists/references and literals, separators/trailing input, depth/recovery, all top-level forms, rejected nested placement, existing annotation syntax, and unsupported call contexts such as `@inline(equals(1, 1))`.
- **Evaluator:** tests beside the new modules for every operator/empty identity, eager errors, absent versus false, nonboolean conditions, missing value positions, unknown names/calls, mixed kinds, large exact integer comparisons, signed minima/overflow, 16/32/64-bit suffixes, float rounding, escapes/bytes/Unicode, and numeric context symmetry.
- **Driver:** proposed `compiler/omega-driver/tests/conditional_compilation.rs`, using the temporary-package pattern in existing `compiler/omega-driver/tests/macro_hygiene.rs`. Cover every declaration family; disabled invalid bodies/signatures/ordinary annotations; unknown macros in disabled bodies; macro definitions/imports/aliases/invocations; ambient/external macros; duplicate alternatives; generated false items containing unknown macros; origin diagnostics; absent primitive/meet/glue registrations; cross-file resolution; and empty output inventory. Assert retained checked identities and exact diagnostic variants/reasons.
- **CLI:** unit tests in `cli.rs` plus proposed `compiler/omgc/tests/compiler_definitions.rs`, following the executable harness in `compiler/omgc/tests/output_layout.rs`. Cover attached/separated/bare forms, identical/mixed-form duplicates, empty/malformed/unused-invalid values, argv strings, option order, same builtin/user spelling, and separate configured invocations. Compile-only target tests cover canonical aliases, hosted/freestanding selection, and all pointer widths without running foreign binaries. Inspect emitted IR to prove removed exported functions/globals are absent before linking.

### Root conformance and specification trace

These are **proposed new direct-child packages under `tests/`**, exercising the real compiler/linker/executable against the new rules in `docs/language/annotations-and-sizeof.md`:

| Case | Rule proved |
|---|---|
| `conditional_compilation` | Absent/false boolean conditions, boolean calls, builtins, and mutually exclusive declarations select expected executable behavior. |
| `conditional_definitions` | CLI booleans, bare `-D`, numbers/floats/chars/strings/bytes select expected declarations. |
| `conditional_pruning` | Disabled semantic errors, unavailable imports/macros, and invalid meet/glue targets are ignored; generated false items skip recursive expansion. |
| `conditional_missing_value` | Missing definition in a value position is diagnosed. |
| `conditional_duplicate` | Duplicate conditions error even when the first is false. |
| `conditional_nested` | A member/method condition is rejected. |
| `conditional_invalid_condition` | Unknown call/wrong kind errors inside otherwise decisive `all`/`any`. |
| `conditional_invalid_syntax` | Disabled source still must parse. |

Use exact `expected.stdout` for executable behavior and `expected.stderr` for intended compile failures. Split negatives when one diagnostic would mask another; keep the larger matrix in component tests.

The existing `bin/test-runner` has no per-case compiler arguments. Add optional `compiler.definitions`: one raw `name[=literal]` per nonempty line, no shell parsing or comment syntax. Prefix each with `-D` and pass separate argv elements only to that case's compilation. Preserve order/duplicates for compiler diagnostics; do not permit arbitrary flags, target/output overrides, or alter prebuilt runtime definitions. These fixtures must not change runtime declarations. Document the format in `docs/architecture/testing-and-validation.md`; custom separate-build configurations belong in CLI integration tests.

### Regression coverage and commands

- Existing suites: parser `macros`, `builtin_macros`, `imports`, `aliases`, `foreign`; driver `macro_hygiene`, `aliases`, `conform`, `gap_visibility`; CLI `output_layout` and separate-linkage tests. Root cases include `t15_annotations_and_sizeof`, `t40e_symbol_invalid_target`, `t40f_symbol_comp_target`, `t40h_symbol_legacy_annotation`.
- Run focused new component tests, then `cargo test -p omega-parser -p omega-hir -p omega-analyzer -p omega-driver -p omgc` for the changed public interfaces. Also run `cargo test --workspace --no-run` to compile migrated driver callers in other crates' test targets.
- After compiler/runtime artifacts are built: `./bin/test-runner conditional_compilation conditional_definitions conditional_pruning conditional_missing_value conditional_duplicate conditional_nested conditional_invalid_condition conditional_invalid_syntax t15_annotations_and_sizeof t40e_symbol_invalid_target t40f_symbol_comp_target t40h_symbol_legacy_annotation`.
- Finish with `just test-all`. The compile-only cross-target CLI matrix supplies target/configuration coverage; no full runtime/platform migration is required.
