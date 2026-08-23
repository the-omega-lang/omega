# Fix foreign linkage vs. non-Omega ABI validation

## Task Description
- **Deliverable:** Correct Omega's semantic ABI validation so `foreign` linkage is no longer treated as synonymous with a non-Omega calling convention. Bare `foreign` functions/function bindings using the Omega convention must accept ordinary Omega by-value values (including structs, unions, named/anonymous enums, fixed arrays, fat pointers, etc.), and non-function `foreign name : Type;` data bindings must not be rejected for having an aggregate/composite type. Keep the current safety restriction for unsupported by-value shapes when the function actually uses a non-Omega convention (`c` or `sysv64`), including indirect calls through such function-pointer types.
- **Purpose:** The language specification intentionally separates two facts: `foreign` controls symbol/linkage behavior, while the resolved function type's `CallingConvention` controls ABI. The analyzer currently violates that separation by running `reject_foreign_aggregate_by_value` for every foreign declaration regardless of its resolved convention, and even for foreign data globals where no function ABI exists. This incorrectly rejects valid Omega-to-Omega ABI boundaries such as `foreign takes_by_value(value: Errors) => void;`. At the same time, the existing declaration-only check does not protect indirect calls through a `foreign(c) (...) => ...` function type, and its hand-written “aggregate” list misses other Omega-specific by-value shapes such as fixed arrays and fat pointers.
- **Chosen direction:** Make ABI validity a property of the **resolved function calling convention and by-value parameter/result shapes**, never of the `foreign` keyword itself.
  - `CallingConvention::Omega`: use Omega's existing `AbiSignature` contract. Any otherwise-valid Omega value type may be passed/returned by value. This includes bare direct `foreign` functions, ordinary Omega-convention function-typed foreign bindings, and Omega definitions in separately compiled objects.
  - `CallingConvention::C | CallingConvention::SysV64`: until Omega has a real per-target aggregate/composite ABI classifier, allow only ABI-scalar shapes by value and reject Omega composite/multi-leaf shapes that the current leaf-flattening ABI would misrepresent externally.
  - A non-function `foreign name : Type;` is an external data symbol, not a call boundary. Do not run function-ABI validation on it.
  - A `foreign(c)` block does not make a typed binding C-convention: `foreign(c) { fp : (S) => S; }` has an ordinary Omega function type and must be accepted; `foreign(c) { fp : foreign(c) (S) => S; }` explicitly names a C function type and remains restricted.
  - The same non-Omega by-value restriction must be checked at an indirect call through a non-Omega function type, because the ABI hazard exists even when no `foreign` declaration is involved.
- **Rejected alternatives:**
  - Do **not** retain a generic “anything under `foreign` rejects aggregates” check. It contradicts the normative FFI model and prevents valid Omega ABI use.
  - Do **not** fix only anonymous enums. The bug is general to foreign/ABI semantics and already affects named aggregates and foreign globals.
  - Do **not** move this validity decision into LLVM/codegen. Semantic invalidity must be rejected in `omega-analyzer`; codegen should continue consuming already-valid `ResolvedFunctionType`s.
  - Do **not** make `foreign(c) (S) => T` function types themselves globally ill-formed merely because they mention an unsupported by-value shape. Such a function pointer can still be stored/passed as a pointer value; reject declarations/definitions/calls that actually rely on that non-Omega signature ABI. This also ensures concrete generic substitution is validated when the call is actually analyzed.
  - Do **not** implement the real C/SysV aggregate classifier as part of this fix. That remains the larger ABI work tracked in `docs/issues/known-issues.md`.

## Technical Details
- **Initial context boundary:**
  - Semantic owner: `compiler/omega-analyzer/src/analysis/`.
  - Normative semantics: `docs/language/foreign-function-interface.md`.
  - ABI architecture: `docs/architecture/abi-and-representation.md`.
  - Current limitation: the “Omega's calling convention is not the platform C ABI” entry in `docs/issues/known-issues.md`.
  - Observable tests: `tests/t19_foreign_function_interface`, `tests/t27_anonymous_enums`, and `tests/t27c_anonymous_enum_boundary_errors` plus focused new FFI error packages described below.
  - No parser/HIR/MIR/codegen representation change is expected. `omega-codegen::abi::AbiSignature` already provides the stable Omega-to-Omega aggregate ABI; this task fixes analyzer gating around it.
- **Affected files/symbols:**
  - `compiler/omega-analyzer/src/analysis/items/mod.rs`
    - Current `reject_foreign_aggregate_by_value` is the source of the over-rejection.
    - `Analyzer::analyze_foreign_binding` currently invokes it for both function-typed bindings and non-function foreign globals.
    - `Analyzer::collect_foreign_function_signature` currently invokes it unconditionally after resolving the convention, including `CallingConvention::Omega`.
  - `compiler/omega-analyzer/src/analysis/calls/mod.rs`
    - `Analyzer::require_callable` is the common path for calls through resolved function values/fields. Use the shared ABI validator here so indirect calls through `foreign(c)`/`foreign(sysv64)` function pointers cannot bypass the restriction.
  - `compiler/omega-analyzer/src/analysis/mod.rs`
    - Register a small shared ABI-validation module if the implementation uses the recommended new `analysis/abi.rs` owner.
  - **Recommended new file:** `compiler/omega-analyzer/src/analysis/abi.rs`
    - Own the semantic predicate for whether a resolved function signature is currently supported by its calling convention, so item collection and call analysis cannot diverge.
    - Keep the type-shape classifier here rather than in codegen; this is accepted-program semantics, not lowering.
  - `compiler/omega-analyzer/src/error/kind.rs` and `compiler/omega-analyzer/src/error/render.rs`
    - Replace/rename `AnalysisErrorKind::ForeignAggregateByValue` so the diagnostic names the actual non-Omega calling convention instead of claiming that `foreign` linkage itself is the problem. Include `CallingConvention` in the error payload.
  - `docs/language/foreign-function-interface.md`
    - Correct the contradictory aggregate paragraph: it currently begins by saying the limitation is for non-Omega conventions but later says “across a `foreign` boundary of any convention.” State explicitly that bare `foreign` uses the Omega ABI and is not subject to the platform-ABI restriction.
    - State that the temporary restriction follows a non-Omega function type at direct declarations/definitions and calls, including indirect calls through a `foreign(cc)` function pointer.
    - State that non-function foreign data bindings have no calling convention and are not subject to function by-value ABI checks.
  - `docs/architecture/abi-and-representation.md`
    - Preserve the existing “one ABI owner” model. Clarify under the external-boundary section that the unsupported case is a non-Omega convention, not foreign linkage; bare `foreign` continues to use the same `AbiSignature` as ordinary Omega calls.
    - The existing “Globals” section already says foreign data globals need no foreign-specific preflight rejection; keep analyzer behavior consistent with it.
  - `docs/issues/known-issues.md`
    - Change “aggregate-by-value across a `foreign` boundary of any convention” to the real limitation: unsupported composite/by-value shapes under non-Omega conventions (`c`, `sysv64`, ...). Keep the future fix as per-target/per-convention ABI classification.
  - Root tests listed in **Testing**.
- **Interfaces/invariants:**
  1. **Linkage and ABI are independent.** `foreign`/mangling/linkage must never be used as a proxy for calling convention. `ResolvedFunctionType::calling_convention` is the semantic source of truth.
  2. **Omega convention is safe for Omega objects.** `CallingConvention::Omega` uses `omega_codegen::abi::AbiSignature` leaf flattening/sret consistently at definitions and calls, including across separately compiled Omega objects. This task must not reject aggregate/composite values merely because the symbol is foreign.
  3. **Non-Omega safety restriction follows actual ABI use.** A direct non-Omega foreign declaration/definition, a foreign function-typed binding with a non-Omega function type, or an indirect call through such a function type must reject currently unsupported by-value shapes.
  4. **Foreign data is not a function boundary.** `foreign global : S;` may bind an aggregate/composite external global. Codegen already emits it as an external byte-array-backed global using Omega's resolved layout. No calling-convention validation applies.
  5. **Do not classify safety by `leaves_of().len()` alone.** A one-leaf source construct can still have non-C source/ABI semantics (for example fixed-array rules differ fundamentally), and a function pointer itself is safe to pass even if its pointed-to signature would be unsafe to invoke. Use an explicit semantic type-kind classifier.
  6. **Current non-Omega by-value safe set:** numeric scalars (`bool`, `char`, signed/unsigned integers, floats, `isize`/`usize`), thin pointers, unknown-size-array thin pointers (`ResolvedType::Array`), and function pointers. `void`/`never` remain valid no-value returns under their existing rules. Pointer-to-composite is safe because the transported value is the thin pointer.
  7. **Current non-Omega by-value unsupported set:** `ResolvedType::SizedArray`, `Slice`, `Str`, `Struct`, `Union`, named `Enum`, `AnonymousEnum`, and `SpecObject`. These use inline/composite or multi-leaf Omega representations for which the compiler has no platform ABI classifier. `ResolvedType::Spec` is not a value type and should remain unreachable/invalid through existing rules rather than being treated as an FFI scalar.
  8. Make the classifier an exhaustive `match` over `ResolvedType` rather than a permissive wildcard. Adding a future value type must force an explicit ABI-safety decision instead of silently becoming accepted at C/SysV boundaries.
  9. `foreign(c) { ... }` applies its convention only to direct signature entries. Typed bindings keep the convention encoded in their own `Type`; analyzer validation must follow the resolved function type and therefore preserve this existing rule automatically.
  10. Diagnostics must say **which convention** is unsupported. Avoid wording such as “cannot cross a `foreign` boundary” because that is the conceptual bug being fixed. A suitable shape is: `'<type>' cannot be passed or returned by value using the 'c' calling convention`, with a note that Omega-convention calls support Omega aggregates but platform aggregate/composite classification is not implemented yet.
- **Out of scope:**
  - Implementing SysV eightbyte classification, AAPCS, Win64 aggregate rules, or a generic platform C ABI classifier.
  - Changing `AbiSignature`, LLVM parameter/result lowering, calling-convention IDs, mangling, or foreign symbol linkage.
  - Defining C-compatible layout for Omega structs/enums/fat pointers; this task only rejects unsupported non-Omega by-value use.
  - Changing foreign block grammar or how its convention is attached in parser/HIR.
  - Introducing implicit C-compatible wrappers or hidden marshaling.
- **Risks/open questions:**
  - **Stop/escalate if codegen evidence contradicts the documented Omega ABI contract.** The current source shows `AbiSignature::build` is shared by definitions/calls and bare `foreign` resolves to `CallingConvention::Omega`; no codegen redesign should be needed. If a focused run shows bare foreign Omega aggregate calls are emitted differently from ordinary Omega calls, investigate that concrete discrepancy before adding another analyzer exception.
  - Do not broaden the accepted non-Omega set merely because an LLVM target happens to lower a test case compatibly. Without a documented per-target classifier, accidental target compatibility is not a language guarantee.

## Implementation Plan
1. **Centralize semantic function-ABI validation in `omega-analyzer`.**
   - Add `analysis/abi.rs` (or an equivalently shared analyzer-owned location) with an exhaustive helper that classifies whether a `ResolvedType` is currently safe by value under a non-Omega convention.
   - Add an `Analyzer` helper that accepts `(id, span, &ResolvedFunctionType)` and:
     1. returns success immediately for `CallingConvention::Omega`;
     2. for `C`/`SysV64`, scans fixed parameters and return type for the first unsupported by-value shape;
     3. reports the convention-aware ABI diagnostic and fails if one is found.
   - Keep variadic-tail promotion/validation separate; this helper concerns declared fixed parameter/result types only.
2. **Fix foreign item collection to use the resolved convention rather than the keyword.**
   - In `Analyzer::collect_foreign_function_signature`, construct the full `ResolvedFunctionType` once the convention/params/return are resolved, then pass that complete type to the shared validator. Bare `foreign` resolves to `Omega` and therefore passes; `foreign(c)`/`foreign(sysv64)` with unsupported by-value shapes remain rejected.
   - In `Analyzer::analyze_foreign_binding`, validate only `ResolvedType::Function(fn_type)` through the same helper. Remove the aggregate check entirely for non-function types so external globals such as `foreign state : S;` are accepted.
   - This automatically gives function-typed bindings the convention carried by their type: `(S) => S` is Omega; `foreign(c) (S) => S` is C.
3. **Close the indirect-call ABI hole.**
   - In `analysis/calls/mod.rs`, invoke the same shared validator when `require_callable` resolves a function value for an actual call. Use the callee/call-site span for the diagnostic.
   - This must reject code such as an ordinary Omega function receiving `fp: foreign(c) (S) => void` and then calling `fp(s)`, even though no `foreign` item appears in that source path.
   - Do not reject merely storing, returning, or passing the function pointer value itself; only the call relies on the pointed-to non-Omega signature ABI.
4. **Correct diagnostic ownership and wording.**
   - Rename `ForeignAggregateByValue` to a convention-based name such as `UnsupportedNonOmegaByValue` / `UnsupportedCallingConventionByValue` and include `{ type, calling_convention }`.
   - Update `Display` and rendered labels/notes/help so they never imply bare `foreign` is unsafe. Mention the concrete convention (`c`/`sysv64`) and point to the existing ABI known-issue entry.
   - Keep the help actionable (use a thin pointer or redesign the external boundary until the platform classifier exists).
5. **Update documentation so future agents do not recreate the bug.**
   - In `docs/language/foreign-function-interface.md`, explicitly state the three-way distinction with examples:
     - `foreign f(S) => S;` — Omega ABI, aggregate/composite by value allowed.
     - `foreign(c) f(S) => S;` — non-Omega ABI, unsupported composite by value rejected for now.
     - `foreign state : S;` — external data symbol, no calling convention involved.
   - Document that `foreign(c) { fp : (S) => S; }` keeps `fp` Omega-convention because a typed binding's function type is authoritative.
   - Document that an indirect call through `foreign(c) (S) => ...` is subject to the same current restriction even without a foreign declaration.
   - Align `docs/architecture/abi-and-representation.md` and `docs/issues/known-issues.md` with that same terminology. Remove the phrase “foreign boundary of any convention.”
6. **Update existing anonymous-enum boundary coverage without making this an anonymous-enum-specific rule.**
   - Keep `tests/t27c_anonymous_enum_boundary_errors`'s negative case as `foreign(c) takes_by_value(...)`; update its comment to say the rejection is caused by the non-Omega convention, not by `foreign` linkage, and update expected diagnostics to the convention-aware wording.
   - Add a positive bare-`foreign` anonymous-enum case to `tests/t27_anonymous_enums` if it can be exercised compactly without obscuring that suite. The general FFI suite below remains the primary owner of the foreign/ABI rule.
7. **Add focused general FFI conformance coverage.**
   - Extend `tests/t19_foreign_function_interface` with a bare direct `foreign` Omega-convention function that takes and returns a nontrivial struct by value and is called at runtime. Prefer a shape large enough to exercise the existing Omega indirect-return (`sret`) path as well as parameter flattening, so the test proves the behavior this analyzer check was incorrectly blocking.
   - In the same positive package, add compile/link-only declarations for:
     - a non-function aggregate foreign global;
     - an Omega-convention function-typed foreign binding with aggregate parameters/results;
     - a typed Omega function binding inside `foreign(c) { ... }`, proving the block convention does not leak into typed bindings.
   - Add a focused negative signature package (for example `tests/t19b_foreign_abi_errors`) for `foreign(c)` and explicitly C-typed foreign bindings using unsupported by-value shapes. Include at least one named aggregate and one currently missed composite shape such as `[N]T` or `*str`, proving the new classifier is broader than the old four-variant match.
   - Add a separate body-analysis negative package (for example `tests/t19c_foreign_abi_call_errors`) containing an indirect call through `foreign(c) (S) => ...` with an aggregate parameter/result. Keep it separate because a rejected signature can prevent body analysis, as already encountered in the anonymous-enum diagnostics suites.
8. **Keep backend behavior unchanged and verify regressions.**
   - Do not modify `omega-codegen::abi`, `llvm::function`, or `llvm::item` unless testing exposes a concrete pre-existing mismatch. Their current contracts already support Omega-convention aggregate transport and external aggregate globals.
   - Run existing convention/codegen tests to ensure scalar C/SysV behavior, variadic promotion, and LLVM convention markers are unchanged.

## Testing
- **New/changed cases:**
  - Analyzer implementation tests for the shared ABI classifier/validator:
    - Omega convention + struct/anonymous enum/composite => accepted.
    - C/SysV64 + scalar/thin pointer/function pointer/unknown-size-array thin pointer => accepted.
    - C/SysV64 + fixed array, slice/string fat pointer, struct, union, named enum, anonymous enum, spec object => rejected.
    - Prefer table-driven coverage and an exhaustive classifier so future `ResolvedType` additions require an explicit decision.
  - `tests/t19_foreign_function_interface` positive runtime case:
    - bare direct `foreign` with aggregate parameter and aggregate return;
    - runtime value survives the call correctly;
    - unreferenced external aggregate global and Omega function-typed binding compile/link;
    - typed binding inside `foreign(c) { ... }` remains Omega-convention.
  - `tests/t19b_foreign_abi_errors` negative signature case:
    - direct `foreign(c)` aggregate/composite by value is rejected;
    - `foreign name : foreign(c) (...) => ...;` with the same unsupported shape is rejected;
    - diagnostic names `c` (or `sysv64` where tested), not a generic `foreign` boundary.
  - `tests/t19c_foreign_abi_call_errors` negative body case:
    - calling a value of type `foreign(c) (S) => ...` with unsupported by-value `S` is rejected even when the function pointer is merely an ordinary parameter/local and there is no `foreign` declaration at that call site.
  - `tests/t27c_anonymous_enum_boundary_errors`: retain `foreign(c)` anonymous-enum rejection with corrected explanation/diagnostic.
  - `tests/t27_anonymous_enums`: add/retain a positive bare-`foreign` Omega ABI anonymous-enum path if concise.
- **Specification trace:**
  - `docs/language/foreign-function-interface.md` rules that:
    1. `foreign` status controls symbol/linkage and is independent from function calling convention;
    2. ordinary function types use the Omega convention even when named by a foreign binding;
    3. bare direct `foreign name(...)` is the Omega-convention foreign form;
    4. typed entries in foreign blocks keep the convention encoded by their type;
    5. only non-Omega conventions are subject to the temporary external by-value ABI limitation.
- **Negative/diagnostic cases:**
  - Expected stderr must assert the convention-aware reason, e.g. that `S` is unsupported by value with `c`, and must not claim that all `foreign` boundaries reject it.
  - Keep signature-phase and body-phase errors in separate packages so signature failure cannot mask the indirect-call check.
- **Regression coverage:**
  - Existing scalar/pointer `foreign(c)` and `foreign(sysv64)` behavior in `t19_foreign_function_interface` remains valid.
  - Existing C variadic promotions and SysV64 non-promotion behavior remain unchanged.
  - Existing external-global emission remains unchanged; this task removes an analyzer rejection to match codegen's current `declare_foreign_binding` support.
  - Existing anonymous-enum C-boundary negative test remains negative; only bare Omega-convention `foreign` becomes valid.
  - No separate-compilation ABI algorithm changed. Existing Omega `AbiSignature` is being unblocked, not redesigned; do not create a new multi-package harness unless a regression reveals a cross-object mismatch.
- **Commands/target coverage:**
  - Focused analyzer tests: `cargo test -p omega-analyzer` (or the narrower ABI test filter once named).
  - Codegen convention regression: `cargo test -p omega-codegen --test convention`.
  - Focused conformance after artifacts are built: `./bin/test-runner t19_foreign_function_interface t19b_foreign_abi_errors t19c_foreign_abi_call_errors t27_anonymous_enums t27c_anonymous_enum_boundary_errors`.
  - Full gate: `just test-all`.
  - `sysv64`-specific new conformance assertions should only be added where the suite already assumes a compatible x86-64 target; otherwise exercise the convention-independent classifier in Rust tests and keep root negative cases on `foreign(c)` for portability.
