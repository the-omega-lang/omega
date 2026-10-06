# `panic$` inside `comp` evaluation is a compile error

## Task Description
- **Deliverable:** when a `comp` evaluation reaches `core::panic::PanicHandler::panic` (normally via `panic$`), evaluation fails with a dedicated diagnostic carrying the panic's message, located at the `panic$` site, with the existing compile-time call trace. Today the same program fails with the misleading `CompErrorKind::ExternCall` ("it calls an 'extern' function") because the gap function has no body.
- **Purpose:** gives `comp` an error mechanism without new syntax, keeping the rule that `comp` "does not create a separate class of functions": one function panics at runtime and fails compilation when evaluated under `comp`.
- **Chosen direction:** the evaluator recognises the handler **by resolved declaration identity**, through the same driver resolution of `core::panic` that runtime checks already use (`compile/runtime_checks.rs::resolve_panic_support`), now cached so a compilation still resolves it at most once. It reads only the `message` field out of the already-evaluated `*PanicInfo` argument. The site is the call's own span, which the macro call-site span rule already makes the `panic$` invocation site, so `PanicInfo`'s `source_file`/`line`/`column` are not read.
- **Rejected alternatives:**
  - A `comp`-only error construct, or Zig-style analysis-time `@compileError`: this duplicates a mechanism, and the analysis-time construct is a different (type-level) feature. Out of scope.
  - Recognising the handler by name/path spelling: this violates the identity rule that `PanicSupport::handler_decl_id` documents.
  - Reclassifying the evaluator's division-by-zero / out-of-range index-or-slice failures as panics: the language spec defines **no** runtime panic for those (only the four `RuntimeCheck`s exist), so calling them panics would invent semantics.
  - Reclassifying the evaluator's invalid-tag sites ("exhaustive match with no matching arm", "try on neither … variant"): `comp_eval.rs` documents them as broken-checked-tree invariants, because interpreter-built values cannot carry an invalid tag. They are not program panics. Leave all of these as they are.

## Technical Details
- **Initial context boundary:** `compiler/omega-analyzer/src/comp_eval.rs` (+ `comp_eval/tests.rs`), `compiler/omega-analyzer/src/resolver.rs` (`ModuleResolver`), `compiler/omega-driver/src/compile/runtime_checks.rs`, `compiler/omega-driver/src/resolver.rs` (`impl ModuleResolver for Driver`), `compiler/omega-driver/src/lib.rs` (`Driver` fields). Docs: `docs/language/compile-time-evaluation.md`, `docs/architecture/types-layout-and-const-eval.md` §"Compile-time evaluator boundary", `docs/architecture/mir-and-codegen.md` §"Runtime checks" (one sentence). `runtime/core/panic.omg` header comment.
- **Affected files/symbols:**
  - `comp_eval.rs`:
    - add `CompErrorKind::Panicked { message: String }` with `Display` = `it panicked: "<message>"`, or `it panicked` when the message is empty (`panic$()` leaves `message = ""`);
    - add a `CompFunctionResolver` method `fn trusted_panic_message_field(&mut self, decl_id: HirId) -> Option<usize>`, defaulting to `None`. It returns the `PanicInfo::message` field index iff `decl_id` is the trusted handler;
    - forward it in `impl CompFunctionResolver for dyn ModuleResolver`;
    - in `eval_call`, on the `Ok(None)` (bodiless) branch, consult it before returning `ExternCall`.
  - `resolver.rs`: the same method on `ModuleResolver`, defaulting to `None`. `analysis/tests.rs::NoResolver` keeps the default.
  - `omega-driver`:
    - add a `Driver` field caching `Option<Rc<PanicSupport>>` (success only);
    - add a success-only cached accessor wrapping `resolve_panic_support`, used both by `bind_runtime_checks` and by the new `ModuleResolver` impl, which compares `decl_id` to `handler_decl_id` and returns `support.message.index`.
  - `error/kind.rs` / `error/render.rs`: no change expected. `CompEvalFailed { reason }` already renders `cannot evaluate this expression at compile time: <reason>`, with the "evaluation stops here" secondary label at the failure span.
- **Interfaces/invariants:**
  - Identity only. A user function named `panic`, or any other bodiless function, still yields `ExternCall`.
  - Resolution happens lazily, only when evaluation reaches a bodiless call. Compilations that never do this, and the runtime-check path, behave exactly as before; `bind_runtime_checks` still reports `RuntimeCheckSupportUnavailable` itself.
  - A resolution failure at `comp` time returns `None` and falls back to `ExternCall`. Do not emit a second diagnostic.
  - Cache only a **successful** resolution. A lazy lookup made during analysis can fail for a transient reason, such as `ResolveError::Cycle` while `core::panic`'s items are still in progress. If that failure were cached, `bind_runtime_checks` would later report a spurious `RuntimeCheckSupportUnavailable` for a `core` that is actually fine. A failed lookup is simply retried by the next caller.
  - The argument shape is `ConstValue::Ref(Struct(fields))`, produced by `AddressOf` at `comp_eval.rs:245`. `fields[message_index]` is the `*str` value; confirm its `ConstValue` form (expected `ConstValue::Str`) with the unit test, not by guessing. A shape mismatch is a broken-tree invariant: report it through `CompErrorKind::Unsupported(..)` like neighbouring sites, never a Rust panic.
  - The span is the call `span` with the interpreter's current `source` and `call_trace`, via the existing `self.err`.
- **Out of scope:** message formatting or interpolation; `@compileError`-style analysis-time errors; rewording the other `Unsupported` diagnostics; any MIR/codegen change.
- **Risks/open questions (escalate, don't decide):**
  - Lazy `resolve_item` of `core::panic` during analysis may meet in-progress queries. The driver reports those as `ResolveError::Cycle`, which the success-only cache absorbs. If instead you observe a Rust panic or an analyzer re-entrancy assertion on that path, stop and report.

## Implementation Plan
1. **Analyzer:** add `CompErrorKind::Panicked` and its `Display`. Add `trusted_panic_message_field` to `CompFunctionResolver` and `ModuleResolver` (both default `None`) and forward it in the `dyn ModuleResolver` impl. In `eval_call`'s `Ok(None)` arm: if the resolver returns `Some(index)`, extract the message from `args[0]` and return `Panicked`; otherwise return `ExternCall` as now. The args are already evaluated before body resolution. Add one short comment at that branch explaining why the bodiless handler is not an extern failure.
2. **Driver:**
   - add the cache field (initialised `None` in `new_with_definitions`) and a `panic_support(&mut self) -> Result<Rc<PanicSupport>, String>` accessor that caches only `Ok`, so a successful resolution happens at most once;
   - switch `bind_runtime_checks` to use it;
   - implement `trusted_panic_message_field` on `Driver`;
   - update the module doc comment of `compile/runtime_checks.rs` (and the "once per compilation" bullet in `mir-and-codegen.md`) to say that `comp` evaluation consults the same cached resolution.
3. **Spec:** in `docs/language/compile-time-evaluation.md`, add a short normative subsection (e.g. "Panics during evaluation") after "Unsupported compile-time operations". It states:
   - an evaluation that reaches `core::panic::PanicHandler` (as `panic$` does) fails;
   - the compile error reports the panic's message at the panic site;
   - the handler is not called and no glue is needed;
   - `defer`s registered by the evaluated functions do not run, matching runtime, where the handler never returns.
   Include a 4–6 line example like the user's `comp { if bad { panic$("…") } else { "OK" } }`.
4. **Architecture doc:** in `types-layout-and-const-eval.md` §"Compile-time evaluator boundary", add one sentence: the resolver also answers whether a bodiless callee is `core`'s trusted panic handler, and evaluation then reports a panic instead of an extern call.
5. **`runtime/core/panic.omg`:** extend the compiler-facing-contract paragraph by one sentence. Compile-time evaluation reads `message` from the `PanicInfo` passed to the handler, so its name and type are part of that contract for `comp` too.

## Testing
- **Unit (`comp_eval/tests.rs`):**
  - a resolver stub returning `Some(k)` for one decl id: calling it with `&Struct{.., message: "boom", ..}` yields `CompErrorKind::Panicked { message: "boom" }`;
  - an empty message yields `it panicked`;
  - the same call with a stub returning `None` still yields `ExternCall`. `calling_an_extern_is_rejected_with_a_precise_reason` must keep passing unchanged;
  - a panic inside a called function carries a non-empty `trace`.
- **Conformance:** new `tests/t50_comp_panic/` with `expected.stderr` (exact output checked). It covers:
  1. a `comp` binding whose block panics directly with `panic$("bad config")`;
  2. a helper function `parse(x: i32) => i32` that panics on bad input, called under `comp`. The diagnostic shows the `panic$` site plus the "required by this compile-time call" trace label;
  3. the same helper called at runtime with good input in `main`, proving one function serves both contexts. This compiles fine but the case fails as a whole, so put this point in a sibling passing case (`t50b_comp_panic_shared_function`) or extend `t14_compile_time_evaluation` with a successful `comp` call of a function that contains an untaken `panic$`.
- **Specification trace:** the new "Panics during evaluation" subsection of `docs/language/compile-time-evaluation.md`.
- **Regression:** `cargo test -p omega-analyzer comp_eval`, `cargo test -p omega-driver`, and `./bin/test-runner t14_compile_time_evaluation t34_comp_generics t34b_comp_generic_errors t45_conditional_compilation` plus any `t46*` never/runtime-check cases (they exercise `bind_runtime_checks`).
- **Full gate:** `just test-all`.
