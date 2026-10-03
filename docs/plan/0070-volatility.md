# Volatile memory access: `core::volatile`

## Task Description

- **Deliverable:** `runtime/core/volatile.omg` declaring two compiler-implemented generic functions, end-to-end support from parser to LLVM, a new normative chapter `docs/language/volatile.md`, and conformance and IR tests.
  ```omega
  exposed read_volatile<T>(location: *T) => T;
  exposed write_volatile<T>(location: *mut T, value: T) => void;
  ```
- **Purpose:** MMIO, interrupt-shared memory, and DMA descriptors need accesses that the compiler never removes, repeats, or merges. Omega cannot express this today without an `asm` block, which is a full compiler barrier and goes through a stack slot. Keeps the no-hidden-behavior and embedded-first invariants.
- **Chosen direction:**
  - Volatility is an operation on a pointer, not a type qualifier. This matches `atomics.md` ("an atomic location is ordinary Omega storage").
  - The two functions are ordinary generic functions as far as name resolution, typing, inference, aliasing, visibility, instantiation, symbols, and taking a function value are concerned.
  - Their declarations have no body. The analyzer supplies each concrete instance's body: one volatile load or store.
  - Recognition is by exact module path plus name, following the `MacroBuiltin` precedent in `omega-parser/src/ast/item.rs`.
- **Rejected alternatives:**
  - `*volatile T` qualifier: doubles every pointer form and hides access counts (`|=`).
  - `asm`-backed platform gaps: full memory clobber plus a stack round-trip on every access. Volatile is a promise the compiler makes, not a platform capability.
  - Bare names `read`/`write`: every exposed core item is an ambient fallback name (`modules-and-imports.md` § "`core` ambient names"; `ambient_core_candidates` in `omega-driver/src/resolver.rs`). A bare `read(p)` with a missing import would silently become a volatile access. **Keep the `_volatile` suffix.**
  - `Volatile::read` on a marker type: adds a constructible type purely as a namespace, and recognition would have to work on methods.

## Technical Details

- **Initial context boundary:**
  - `docs/language/atomics.md` (model for chapter tone and structure).
  - `docs/language/grammar.md` § Functions.
  - `docs/architecture/mir-and-codegen.md` § Place representation, § Naked functions.
  - Crates: `omega-parser` (functions parser and AST), `omega-hir` (`HirFunctionDef`), `omega-analyzer` (`analysis/items/bodies.rs`, `checked.rs`, `layout.rs`, `comp_eval.rs`), `omega-driver/src/compile/signatures.rs`, `omega-mir/src/lower/`, `omega-codegen/src/llvm/{expr,place}.rs`.
- **Affected files/symbols:**
  - **Parser** (`omega-parser/src/ast/statement.rs`, `parser/item/functions.rs`):
    - `FunctionDefinitionStmt.codeblock` becomes `Option<CodeblockExpr>`.
    - Only the item-level path (`functions.rs`, the `Item::FunctionDefinition` arm, around line 20) may accept `;` in place of a block.
    - Method, conformance, and glue parsing (`parser/item/definitions.rs` call sites at ~132 and ~374) still require a block. The cleanest way is probably a flag or wrapper on `parse_function_definition`.
    - The macro expander (`macros/expander.rs:360`) maps the `Option`.
  - **HIR:** `HirFunctionDef.body` becomes `Option<HirBlock>`. Update `omega-hir/src/lower/item.rs` (~272: the span must still be computed when there is no body; end it at the signature or `;`).
  - **Analyzer classifier (new):** one pure function, e.g. `fn compiler_function(module_path, &HirFunctionDef) -> Result<Option<CompilerFunction>, AnalysisErrorKind>`, with `enum CompilerFunction { ReadVolatile, WriteVolatile }`. It is the single owner of the rule. Put it in a new `omega-analyzer/src/compiler_functions.rs` or next to the items code. Behaviour:
    - Body present and not a recognized path: `Ok(None)`.
    - Body absent and not a recognized path: error `FunctionWithoutBody { name }`.
    - Recognized path (`["core","volatile"]` + name) with the wrong shape: error `MalformedCompilerFunction { name }`. Wording mirrors the parser's `MalformedBuiltinDeclaration`.
    - Recognized path with the right shape: `Ok(Some(..))`.
  - **Recognized shape, checked syntactically on HIR:**
    - Visibility is `exposed` and there are no annotations.
    - Exactly one unbounded type parameter `T` with no default, and `self_mode` is `None`.
    - `read_volatile(location: *T) => T` or `write_volatile(location: *mut T, value: T) => void`.
    - No body.
    - A recognized name that *has* a body is also malformed. The rule is about the declaration, as in `bind_definition` in `omega-parser/src/macros.rs`.
  - **Driver eager sweep:** in `omega-driver/src/compile/signatures.rs` (~265–280, the loop over every `HirItem::FunctionDefinition` per module that already calls `normalized_function`), call the classifier and record its error. This is what rejects a bodyless user function even when it is generic and never instantiated. Confirm the sweep covers every module, including the `core` extern; if it does not, escalate rather than add a second rejection site.
  - **Analyzer body synthesis** (`analysis/items/bodies.rs::check_function_body`):
    - When `f.body` is `None`, call the classifier. It must yield `Some`, because the sweep already rejected everything else; treat `None` there as an internal invariant, not a user error.
    - Analyze parameters normally, then build a `CheckedBlock` whose tail is the new expression over the `location` parameter's deref place, mirroring how `check_naked_function_body` builds its block.
    - The instance is concrete at this point, so `T` is resolved.
  - **Checked tree** (`checked.rs`): add `CheckedExpr::VolatileRead(CheckedPlace)` and `CheckedExpr::VolatileWrite(CheckedAssignment)`. The place is `*location` (a `CheckedProjection::Deref` over the parameter). Update every exhaustive match the compiler flags: `dead_code.rs`, `runtime_checks.rs`, `analysis/patterns.rs` and the others reported.
  - **Compile-time evaluation** (`comp_eval.rs::eval_expr`): both variants return `CompErrorKind::Unsupported("a volatile access")`, mirroring the `InlineAsm` arm (~972).
  - **Layout** (`omega-analyzer/src/layout.rs`): add `pub fn volatile_alignment(ty, pointer_bytes) -> u32`.
    - Primitive scalars (integers, `isize`/`usize`, floats, `bool`, `char`), thin data pointers, and function pointers: `max(type_alignment(ty), size)`.
    - Every other type: `type_alignment(ty)`.
    - Match on `ResolvedType` kinds; do not infer "scalar" from `leaves_of`, so a single-field struct deliberately stays on `alignof`.
  - **MIR** (`omega-mir/src/body.rs`, `lower/`): add `MirExpr::VolatileRead(MirPlace)` and `MirExpr::VolatileWrite(MirAssignment)`, lowered from the checked variants through the existing `lower_place` and assignment paths. `MirPlace.align` keeps `place_align`, which MIR computes without pointer width; codegen applies the volatile rule.
  - **Codegen** (`omega-codegen/src/llvm/expr.rs`, `place.rs`, plus the `storage.rs` scan):
    - Emit through the existing leaf loops (`load_scalars` / `store_scalars`), with every emitted load or store marked `set_volatile(true)`.
    - Alignment is `layout::volatile_alignment(ty, pointer_bytes)` per access, using the existing `offset_align` for multi-leaf types.
    - A zero-sized `T` emits nothing (no leaves).
    - Prefer a `volatile: bool` parameter on the leaf helpers over copying the loops.
  - **Runtime:** new `runtime/core/volatile.omg` containing only the two declarations, with a one-line header pointing to `docs/language/volatile.md`.
- **Interfaces/invariants:**
  - **Access count and order:** each call performs its accesses exactly once; they are never removed, repeated, merged, or reordered against other volatile accesses. A primitive scalar or thin pointer that the target accesses in one instruction is one access; anything wider is split in an unspecified way.
  - **Alignment:** as `volatile_alignment` above. A misaligned address is a program error.
  - **No synchronization:** not atomic; ordinary memory is not ordered against volatile accesses.
  - **Ordinary functions otherwise:** generic instances get ordinary symbols and weak linkage, and taking `read_volatile<u32>` as a function value works. No `@inline` (it currently warns `InlineNotEnforced`); optimization inlines the call.
  - **No privilege leak:** no runtime, platform, libc, or allocation involvement. The only new compiler privilege is the two recognized declarations.
- **Out of scope:**
  - Fences of any kind.
  - Secure zeroing or bulk volatile operations.
  - A `std` `Volatile<T>` wrapper.
  - Bodyless functions anywhere except item-level free functions.
  - Changing ambient lookup.
- **Risks/open questions (escalate, do not decide alone):**
  - The eager sweep turns out not to reach generic templates or `core`.
  - Generic instances of an extern-package function are not emitted into the calling unit the way the plan assumes.
  - The `Option` body change spreads beyond the sites listed here into method or spec machinery in a way that needs a structural decision.

## Implementation Plan

1. **Parser and HIR:** make the body optional (AST `codeblock`, HIR `body`), accepting `;` only on item-level free functions. Fix the expander and HIR lowering. Existing callers that hit `None` should be unreachable for now (`expect` with a reason) until step 3.
2. **Classifier and diagnostics:** add the classifier, the two `AnalysisErrorKind` variants with rendering (`error/kind.rs`, `error/render.rs`), and the driver eager-sweep call. Tests: unit tests for the classifier, plus a driver test showing that a user bodyless function, including an uninstantiated generic one, is rejected.
3. **Checked tree and synthesis:** add the checked variants, synthesize bodies in `check_function_body`, handle them in `comp_eval`, and fix the exhaustive matches.
4. **Layout:** add `volatile_alignment` with unit tests: `u32` gives 4; `usize` gives the pointer width, so 2 on AVR and 8 on x86-64; a packed struct gives 1; an `@layout(align = 8)` struct gives 8.
5. **MIR and codegen:** add the MIR variants and lowering, and volatile leaf emission with the alignment rule.
6. **Runtime:** add `runtime/core/volatile.omg`.
7. **Docs:**
   - New `docs/language/volatile.md`, structured like `atomics.md`: accepted forms, typing, access semantics, alignment, non-synchronization, `comp` rejection, the zero-sized case, why the `_volatile` names exist (ambient lookup), and a short rejected-designs section. List it in `docs/language/README.md`.
   - `grammar.md` § Functions: `( ";" | code-block )`, with a sentence that a bodyless function is valid only for the compiler-implemented declarations in `volatile.md`.
   - `functions.md`: one cross-reference sentence.
   - `docs/architecture/runtime-and-platform.md` § `runtime/core`: add the compiler-implemented `core::volatile` declarations to the owned list and the "Compiler privilege" list.
   - `docs/architecture/mir-and-codegen.md`: a short "Volatile access" note covering the MIR variants and who decides alignment.
   - `docs/guide/core-library.md`: a `core::volatile` entry with a small MMIO-style example (check the syntax against `docs/guide/quick-reference.md`).
   - `quick-reference.md`: one line.

## Testing

- **New/changed cases:**
  - **Codegen IR** (`omega-codegen/tests/`, new `volatile.rs` modelled on `alignment.rs`; register `runtime/core` as an extern the way `omega-driver/tests/range.rs` does):
    - Two consecutive `read_volatile` calls and two consecutive `write_volatile` calls produce IR containing `load volatile` and `store volatile` instructions with `align 4` for `u32`.
    - A packed struct gets `align 1`.
    - A zero-sized `T` produces no volatile instruction.
    - Check at least one 16-bit pointer target, reusing the targets in `alignment.rs::alignment_survives_narrow_pointer_targets`.
  - **Conformance `tests/t48_volatile/`:**
    - Round-trips `u8`, `u32`, `u64`, `f64`, `bool`, and a thin pointer through `read_volatile` and `write_volatile` on naturally aligned storage.
    - A struct value through an `@layout(align = 8)` struct.
    - Generic inference from the argument, plus an explicit `read_volatile<u32>`.
    - Taking `read_volatile<u32>` as a function value and calling it.
    - A zero-sized `T` (`void` or a marker) completes.
    - Checked with `expected.stdout`.
- **Specification trace:** each case maps to a rule in `docs/language/volatile.md`: typing and inference, value round-trip, function-value use, the zero-sized case.
- **Negative/diagnostic cases (each with `expected.stderr`):**
  - `tests/t48b_volatile_errors/`: `read_volatile` reached during `comp` evaluation reports the unsupported-volatile error; writing through an immutable pointer (`write_volatile` with a `*T`) is a type mismatch.
  - `tests/t48c_function_without_body/`: a user bodyless function, generic and never called, is rejected with `FunctionWithoutBody`.
  - Malformed core declarations cannot be reached from the root suite; cover `MalformedCompilerFunction` with classifier unit tests.
- **Regression coverage:**
  - Parser tests: function, method, glue, and `foreign` parsing.
  - `omega-codegen/tests/alignment.rs`.
  - `tests/t24_naked_functions`, `t14_compile_time_evaluation`, `t35_atomics`, `t10_generics`, `t05h_generic_function_values`.
- **Commands:** `cargo test` in the touched crates; `./bin/test-runner t48_volatile t48b_volatile_errors t48c_function_without_body t24_naked_functions t35_atomics`; then `just test-all`.
