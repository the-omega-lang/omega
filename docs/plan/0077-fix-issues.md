# Known-issues batch 1: settled-semantics bug fixes

## Task Description
- **Deliverable:** fix the verified, still-live entries from `docs/issues/` (plus one closely related untracked bug found during triage) whose correct behavior is already settled by the specification or by an existing compiler mechanism, and bring the issue files back in line with reality (remove what this batch fixes, and remove entries that triage found already stale).
- **Purpose:** every item is a compiler crash, an unsound acceptance, a misleading diagnostic, or a warning on a trivial program. None of them needs a language-design decision, and each fix is local to its owning crate.
- **Chosen direction:** one bounded fix per item, each in its owning subsystem, with independent tests. Items are ordered so that each step builds and passes on its own; do them in order and do not merge steps.
- **Rejected alternatives (matter to review):**
  - `&function_declaration` is **rejected with a diagnostic**, not materialized as a static slot holding the address. A function value already *is* its address (`strings-casts-arrays-and-slices.md`, "Function values and thin raw pointers"). No storage exists to point at, and `ConstValue` has no function-address variant. Materializing one would be a new feature, and it would be a false friend for C programmers, for whom `&f == f`.
  - `defer` in a repeated loop header is **rejected** like `defer` in a loop body (`DeferInsideLoopNotSupported`). Per-iteration defer instances are a language change and are out of scope.
  - The index domain is **any integer type**, which is what codegen already handles and what existing code relies on (`a[u64]`, `a[u8]` and `a[usize]` compile and run today). Narrowing it to `i32` to match slice bounds would break code, so it is left for the user to decide (see Risks).
  - The `loop` expression is a **parser/HIR desugaring** to a codeblock holding the existing `loop` statement, not a new `HirExpr`/`CheckedExpr` variant. Block typing already gives `never` when the final statement diverges and `void` otherwise, which is exactly what `control-flow-and-operators.md` § `loop` and `types-and-primitives.md` § never require. A new variant would touch five exhaustive matches for no semantic gain.

## Technical Details
- **Initial context boundary:** per step, only the named crate. Language authority is the chapter named in each step. Read `docs/guide/quick-reference.md` before writing `.omg`. Test conventions are in `docs/architecture/testing-and-validation.md` (`expected.stderr` for failed compiles, `expected.compiler.stderr` for warnings on successful compiles).
- **Affected files/symbols (verified):**
  1. **Slicing never marks its root used** (untracked; it is the cause of `unused variable 'buf'` on every `println$`):
     - `omega-analyzer/src/analysis/places/slicing.rs::analyze_slice` calls `analyze_place`. `analyze_place` (`places/mod.rs`) marks a root used only when the place has projections, and `buf[0..]` has none. Slicing reads the root (its address, or its fat pointer), so `analyze_slice` must mark a `CheckedPlaceRoot::Variable` root used.
     - Separately, `runtime/std/io.omg` macros `print`/`println`/`eprint`/`eprintln` (check the full list there) discard `BufWriter::flush`'s `Option<usize>`. Write the discard explicitly (`<void>out.flush();`, per the `unused_return_value` help text). Keep current behavior; this is not a change to flush-failure policy.
  2. **`&function_declaration` crashes codegen** (`llvm/place.rs`, "a function reference is never itself further-projected"):
     - In `omega-analyzer/src/analysis/exprs/mod.rs::analyze_address_of_inner`, a place whose root is `CheckedPlaceRoot::Variable { storage: Storage::Function, .. }` with no projections is rejected with a new `AnalysisErrorKind` (add it in `error/kind.rs` and `error/render.rs`). The help text says the function value is already its address: use it directly, or cast with `<*void>name` for raw data. Apply the same rule to `&mut`.
     - Check the const path too: `analysis/consts.rs` handles `HirExpr::AddressOf` at about lines 73 and 241, and `comp_eval.rs` asserts that a "Storage::Function place is never itself projected". Function-typed *bindings* (locals, globals, parameters) keep working: `&indirect` where `indirect: () => void = stop` is valid.
  3. **`defer` in a repeated loop header**:
     - `omega-analyzer/src/analysis/stmts.rs`: the `HirStmt::While` condition (about line 303) and `analyze_for`'s condition/post (about lines 397/423) are analyzed outside `with_loop`, so the `HirStmt::Defer` check (`!self.loop_stack.is_empty()`) misses them. Add a dedicated analyzer flag or counter (next to `in_defer_body` / `loop_stack` in `analysis/mod.rs`) that is set while analyzing those repeated header expressions, and make the `Defer` arm reject when it is set, using `DeferInsideLoopNotSupported`.
     - Do **not** push the loop onto `loop_stack` for the header: that would change `break`/`continue` targeting. The `for` initializer and a `for x in expr` iterable are evaluated once and stay unaffected.
  4. **Index expression type unchecked** (a float or `bool` index panics in `llvm/place.rs` `into_int_value`):
     - In `omega-analyzer/src/analysis/places/fields.rs::project_index`, after analyzing the index, require an integer type (signed or unsigned, any width including `isize`/`usize`). Otherwise report a new `InvalidIndexType { r#type }` error. Keep the `Expected::None` analysis so unsuffixed literals keep their current default.
     - Check whether the comp/const indexing path (`consts.rs` / `comp_eval.rs`) needs the same check, and if so route it through one helper rather than duplicating it.
     - Spec: add the rule to `docs/language/strings-casts-arrays-and-slices.md` § Indexing and slicing. An index is any integer-typed expression; slice bounds remain `i32`, as `analyze_slice`'s `analyze_bound` already enforces. State both rules plainly and do not editorialize about the asymmetry.
  5. **Number-literal suffix resolved as a named type** (`alias u7 = u8; 10u7` compiles, and macro-authored suffixes resolve in the caller's module):
     - `omega-analyzer/src/analysis/literals.rs::analyze_number` and `analysis/consts.rs::const_number` (about line 136) call `resolve_type(Type::Named(suffix))`. Replace both with one lookup against the builtin numeric primitives: add a small `pub(crate)` helper beside `BUILTIN_TYPE_NAMES` in `context.rs` (or reuse the `Context::new` table) mapping a suffix spelling to its numeric `ResolvedType`.
     - A shape-valid but non-primitive suffix (`u7`, `i9`, `f13`, `u1024`) gets the existing `invalid numeric type` diagnostic even when a same-named declaration exists. This removes the module/`reveal`/origin dependency from both sites. Spec authority: `docs/language/lexical-structure.md` § Numeric suffixes (already correct; no doc change).
  6. **Reading or writing another package's data global crashes codegen** (`llvm/place.rs`, "mir body guarantees ... was declared as a global"):
     - Extern *functions* flow through `omega-driver/src/compile/output.rs::collect_extern_functions` → `ExternFunctionRef` (`omega-analyzer/src/checked.rs`) → `omega-driver` `error.rs` program struct field `extern_functions` → `omega_codegen::CodegenRequest::extern_functions` (`request.rs`) → `catalog.rs::Catalog::build`, which declares them using `omega_mir::mangle::extern_function_ref_symbol`. Globals have no such channel.
     - Add the parallel one: an `ExternGlobalRef { decl_id, module_path, name, type, symbol policy, foreign: bool }` in `checked.rs`, collected in the same driver sweep from `ResolvedItem::Value` items in extern modules whose storage is the global storage class (verify which `Storage` value foreign *data* bindings use; function-typed foreign bindings already route as functions). Thread it as `extern_globals` through the program struct, `omgc`'s request construction and `CodegenRequest`, and push it into `Catalog::globals` as an external declaration with no initializer.
     - **Symbol identity must have one owner.** The consumer's symbol must equal what the defining package emits: `omega-mir/src/lower/item.rs::global_symbol` for definitions and `foreign_binding_symbol` for foreign data bindings. Factor the non-function arms of those two into a `pub fn` in `omega-mir/src/mangle.rs` (next to `extern_function_ref_symbol`), used by both lowering and the extern path. Do not re-derive the policy in codegen.
  7. **`loop` is not parsed in expression position**:
     - In `omega-parser/src/parser/expression.rs`, the primary-expression dispatch that already handles block-bodied expressions (`if`/`match`/codeblock) should accept `loop { ... }` and produce an `Expression::Codeblock` whose contents are the single existing loop statement (reuse the statement parser's loop parsing; do not duplicate it).
     - Keep statement-position `loop` unchanged. Confirm `Analyzer::block_type` / `stmt_diverges` give `never` for a `loop` with no `break` targeting it and `void` otherwise. Spec authority: `control-flow-and-operators.md` § `loop`, `types-and-primitives.md` (line about "`loop` expressions with no reachable `break`"); no doc change unless the grammar section lists expression forms.
  8. **`glue` functions cannot carry annotations**:
     - `omega-parser/src/parser/item/definitions.rs::parse_glue_def` passes `Vec::new()` annotations and loops only on `Ident`. Mirror `parse_member_functions` (same file, about line 113): loop on `Ident | @`, call `parse_annotations`, report `AnnotationWithoutItem` for a dangling list, and pass the annotations to `parse_function_definition`.
     - Then make sure semantic analysis applies them to glue functions the same way as to ordinary functions (`@suppress`, `@inline` family, `@naked`), and **rejects** `@symbol` (a glue's identity is the gap's glued symbol) and `@cond` (a glue fills its whole gap or nothing). Reuse the existing annotation-applicability error kinds if one fits.
     - Spec: add a short "Annotations on glue functions" rule to `docs/language/gaps-and-glue.md`.
  9. **Macro repetition separator accepts tokens that cannot survive substitution**: `omega-parser/src/parser/macro_syntax.rs::parse_repetition` must also reject a `$` token and a macro-variable token as the separator, with `InvalidMacroSeparator` at the definition. Check the lexer for the exact `TokenKind` of `$name`.
  10. **Macro expansion depth**: `omega-parser/src/macros/expander.rs` has only a total `budget` (`MAX_EXPANSIONS = 256`, `macros.rs`). `omgc` runs on a 256 MiB worker stack (`omgc/src/main.rs`), so the CLI already reports cleanly. The parser crate on an ordinary 2 MiB thread does not. Add a nesting-depth limit, entered and exited around each invocation expansion in both the expression path and `expand_statements_invocation`, reported through the existing `ExpansionLimitExceeded` path with depth wording. Keep the total budget. Pick a depth that the existing recursive-macro test suites stay well under, and set it from evidence: run the parser crate's macro tests on a default-stack thread.
  11. **`Display for ParseErrorKind` builds a whole `Diagnostic`** (`omega-parser/src/diagnostics.rs` about line 267): split the headline text out of `ParseError::to_diagnostic` into a message-only function that `to_diagnostic` itself calls, so the wording still has exactly one definition site.
  12. **`x86_64-windows` cannot resolve `_fltused`**:
      - Define it in the Windows platform as a module-level storage binding with `@symbol(name = "_fltused")`, restricted to x86_64 with `@cond`. It goes in `runtime/plat/os/windows/`, which is shared with `aarch64-windows`, where the symbol must not appear. Check `docs/language/annotations-and-sizeof.md` for the arch `@cond` spelling. C convention gives it type `i32` and value `0`; the linker only needs the definition.
      - Extend `bin/check-platform`'s Windows block (about line 538) so it also proves that no `core`/`std`/`plat` object for `x86_64-windows` leaves `_fltused` undefined after the platform defines it.
  13. **Issue-document hygiene** (docs only):
      - Remove every entry this batch fixes.
      - Remove entries triage found already resolved: "A generic type prints without its arguments" (now prints `Box<i32>`); "`HirBlock::span` is still carried without a downstream reader" (read in `analysis/patterns/mod.rs`); the build-reproducibility bullet in `language-limitations.md` § Visibility; and "Macro-authored unused locals are not linted" (they are linted and attributed to the macro definition, as `println$` showed).
      - Rewrite the "unfilled gap warns on every compilation" entry: the behavior is unchanged (`omega-driver/src/compile/signatures.rs::sweep_gaps` warns for every unglued gap regardless of use), but the `core::atomic` example is stale because every shipped platform now glues atomics.
      - Collapse exact duplicates to one canonical entry with cross-links: C-ABI aggregates (known-issues ×2, language-limitations ×1); AVR `-O0` (known-issues, compiler-limitations); `*str` UTF-8 (known-issues, language-limitations); float formatting (known-issues, language-limitations); hand-written macro traversal (known-issues, compiler-limitations).
      - Do not otherwise restructure `docs/issues/`.
      - **Packed-by-default layout is settled as the intended, platform-uniform default** (user decision). Replace the design-debt entry "Packed-by-default layout has no per-target safety argument" with the actual argument, placed in `docs/architecture/types-layout-and-const-eval.md`:
        - The layout default is identical on every target by design.
        - Codegen gives every load, store, stack slot and global its Omega effective alignment from `omega_analyzer::layout::type_alignment` (`omega-codegen/src/llvm/place.rs`, `expr.rs`, `mod.rs`, `item.rs`), so LLVM never assumes more alignment than exists. On strict-alignment targets it emits alignment-safe access sequences.
        - The cost of the default is performance, not correctness. `@layout(align = n)` is the opt-in.
        - Atomics and volatile keep their natural-alignment contracts.
      - Then delete that design-debt entry. Drop the "target-safety caveats" pointers in `docs/language/types-and-primitives.md` (§ packed default, about line 198) and `docs/language/structs-and-unions.md` (about line 56). The rule itself stays unchanged.
- **Interfaces/invariants:**
  - No ABI or mangling changes. Step 6 must make the consumer's symbol byte-identical to the definer's.
  - No new runtime cost.
  - Diagnostics are produced before MIR (architecture invariant 7). Codegen gets no workaround for steps 2 or 4.
  - Determinism: collect `ExternGlobalRef`s in the same deterministic order as extern functions.
  - `CodegenRequest` gains a field; that is acceptable pre-1.0.
- **Out of scope:**
  - Every other `docs/issues/` entry, in particular: conformance mangling's spec module path, the C-ABI aggregate classifier, generic foreign functions, the field/method namespace decision, analyzer generic-resolution gaps, driver/HIR API reshaping, and runtime library additions. Those are later batches or need user decisions.
  - Per-iteration `defer`.
  - Unifying the index and slice-bound domains.
- **Risks/open questions (stop and escalate rather than decide):**
  - Step 4: if any `runtime/` or `tests/` source indexes with a non-integer type that currently compiles and runs (it should not), stop and report.
  - Step 6: if the driver cannot tell a foreign data binding from a definition at collection time without a resolver/interface change, stop and report the needed interface.
  - Step 7: if a `loop` expression conflicts with an existing contextual use of the word `loop` (check `parser::contextual`), stop.
  - Step 8: if the analyzer's annotation validation cannot see glue functions without a HIR shape change, stop.

## Implementation Plan
1. **Slice marks root used + std console macros.** Edit `analyze_slice` and `runtime/std/io.omg`. Add an empty `tests/t00_hello_world/expected.compiler.stderr` so a warning-free hello world is gated from now on. Add a focused analyzer/driver test that `&mut a[0..]` on an otherwise-unread local does not warn.
2. **Reject `&function_declaration`.** Add the error kind and rendering, the analyzer check, and the const-path check.
3. **`defer` in loop headers.** Add the analyzer flag and rejection.
4. **Index type.** Add the analyzer check, the error kind, and the spec sentence.
5. **Literal suffix.** Add the shared primitive lookup and use it at both sites.
6. **Extern globals.** In order: shared symbol function in `omega-mir::mangle` (refactor lowering to use it, no behavior change) → `ExternGlobalRef` plus driver collection → program/request plumbing in `omgc` → catalog declaration.
7. **`loop` expression.** Parser desugaring plus tests.
8. **Glue annotations.** Parser, then analyzer validation, then the spec rule.
9. **Separator validation.**
10. **Expansion depth limit.**
11. **Message-only parse-error text.**
12. **`_fltused` and the `check-platform` assertion.**
13. **Issue-document hygiene.** Last, so that removals match what actually landed.

## Testing
- **New/changed cases (root conformance, `tests/<case>/`; use the next free numbers, e.g. `t54*`):**
  - **Step 1:** `t00_hello_world/expected.compiler.stderr` (empty).
  - **Step 2:** a negative case. `&f` and `&mut f` on a function declaration give the new error, with full `expected.stderr`. A positive companion: `&binding` of a function-typed local, called through the pointer, prints its output.
  - **Step 3:** a negative case with `defer` inside a codeblock in a `while` condition and in a C-style `for` condition and post, with full `expected.stderr`. Defer in a `for` *initializer* still compiles and runs once.
  - **Step 4:** a negative case with `f32` and `bool` indices, with full `expected.stderr`. A positive case: `u8`/`u64`/`usize`/`i64` indices read the right elements.
  - **Step 5:** a negative case with `alias u7 = u8; 10u7`, giving `invalid numeric type` in full `expected.stderr`. Plus a macro-body suffix case proving that the caller's alias is not picked up.
  - **Step 7:** a positive case with `x: i32 = if c { 1 } else { loop {} };`, a call argument `f(loop { break; })` (or similar) that type-checks as `void`, and a `never`-returning function whose tail is a `loop` expression. Add a negative case if `loop { break; }` is used where `i32` is expected, with full `expected.stderr`.
  - **Step 8:** a positive case with `@suppress(unused_parameter)` on a glue function (assert empty `expected.compiler.stderr`). A negative case with `@symbol` and `@cond` on glue functions, with full `expected.stderr`. Extend `t17_gaps_and_glue` if a sibling case is not cleaner.
  - **Step 9:** a negative case with `$...($x)` and `$...($)` separators, diagnosed at the definition, with full `expected.stderr`.
- **Crate tests:**
  - **Step 6:** an `omgc` separate-compilation test next to `compiler/omgc/tests/separate_linkage.rs` / `global_mangling_linkage.rs`. A provider package defines `exposed mut counter : i32 = 5;` and `exposed foreign mut fcounter : i32;` (with the C-side or Omega-side definition the existing tests use). The consumer reads and writes both, the program links and runs, and the symbol names match the definer's objects. Also cover `@symbol(name = ...)` and `mangle = disabled` globals.
  - **Step 10:** a parser-crate test that a self-recursive macro reports the depth error on a default-size thread (spawn with an explicit 2 MiB stack).
  - **Step 11:** the existing parser diagnostics tests must stay byte-identical.
  - **Step 12:** `bin/check-platform`.
- **Specification trace:**
  - Step 2: `strings-casts-arrays-and-slices.md` (function values are addresses).
  - Step 3: `control-flow-and-operators.md` line on defer-in-loop.
  - Step 4: new indexing sentence.
  - Step 5: `lexical-structure.md` § Numeric suffixes.
  - Step 6: `modules-and-imports.md` (imported exposed globals are usable).
  - Step 7: `control-flow-and-operators.md` § `loop`.
  - Step 8: new `gaps-and-glue.md` rule.
  - Step 9: `macros.md` repetition grammar.
- **Regression coverage:**
  - `t04_control_flow`, `t17_gaps_and_glue`, `t18*` macro cases, `t12*` import cases, `t31_hex_literal_suffix_errors`, `t44h_dynamic_index_order`, `t46*` never cases.
  - Every case with `expected.compiler.stderr`, since step 1 changes warnings that runtime macros produce.
  - `cargo test -p omega-parser -p omega-analyzer -p omega-driver -p omega-mir -p omega-codegen -p omgc`.
- **Commands:**
  - After each step: `cargo build`, then the focused `./bin/test-runner <cases>`.
  - Rebuild runtime objects (`just build-runtime`) after steps 1 and 12.
  - At the end: `just test-all`, `just check-platform`, `cargo fmt --check`, `git diff --check`.
