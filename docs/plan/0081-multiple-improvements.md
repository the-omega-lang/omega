# Remaining issues (phased) — revision 2

## Task Description
- **Deliverable:** the remaining phases of the "Remaining issues" plan, each committed on its own with the full gate green:
  - **Phase 8a (new):** a generic owner's concrete members are checked and emitted only when used.
  - **Phase 8b (revised Phase 8):** the `Option`/`Result` constructors and callback-free methods, except `as_ptr`/`as_mut_ptr`.
- **Status of revision 1 (2026-10-23):**
  - **Done and committed:** Phases 2, 3, 4, 5, 6, 7 and 9. Their work is in the tree; do not redo it.
  - **Blocked: Phase 1** (positional annotation holes). Named `$Hole{n}` parameters survive alias expansion: they keep a hole that `alias Duo<T> = Pair<T, T>` duplicates consistent, report the `UninferredHole` when an alias drops one, and let alias bounds be re-checked on the solved annotation. Positional holes plus `pattern_type` lose all three. A fix needs alias expansion in pattern space, which is an unsettled design question. Out of scope here.
  - **Blocked, then reverted: Phase 8.** Instantiating a generic owner checks and emits every concrete member. So `core`'s own `Option<u8>`/`Option<u16>`/… put unused `unwrap`/`is_some` copies into every package object. Those copies reference `core::panic::PanicHandler`, through `panic$` and through the compiler's invalid-tag check, which breaks links without `--gc-sections` or panic glue (`compiler/omgc/tests/aligned_allocation_glue.rs`). Separately, member *signatures* are resolved eagerly, so `as_ptr(*self) => Option<*T>` on `Option<T>` never terminates.
- **Purpose:** an abstraction must not cost code, link obligations, or runtime support that nothing uses. Freestanding builds must not acquire a panic-handler requirement from `core` methods they never call.
- **Chosen direction (8a):** treat an instantiated owner's concrete member exactly like an instantiated generic member template is treated today:
  - It has its own method `ItemKey`, its body is checked by `Driver::check_method_body`, and it is emitted as a standalone `CheckedFunctionDef` carrying `method_owner` into `generic_instantiations`.
  - It is emitted only when some emitted body references it. Instantiating the owner (its type, layout, and member signatures) no longer checks member bodies.
- **Rejected alternatives:**
  - **Requesting the body at every analyzer site that names a member** (method calls, `Type::name`, `Type::self::name`, overloads, function values). There are many construction sites, and missing one is a link error. A single reachability pass over emitted checked code cannot miss a reference form.
  - **Dropping unused members in MIR.** Their bodies would still be checked, which reports errors and requires panic support (`bind_runtime_checks`) for code that is never emitted.
  - **Making member signatures lazy in this phase.** That is a deeper TypeStore/member-lookup change (`types.functions(nominal)` is read by lookup, overloads, conformance checking and vtables). It is only needed for `as_ptr`/`as_mut_ptr`, so it is recorded instead.

## Technical Details
- **Initial context boundary (8a):** `compiler/omega-driver` (`items/resolution.rs` `ensure_item_at`, `bodies.rs` `ensure_item_body`/`check_item_body`/`check_aggregate_body`/`check_method_body`, `items/methods.rs` `instantiate_method_template`, `items/mod.rs` `ItemQueries`, `compile/mod.rs` emission loop, `compile/bodies.rs` `drain_pending_declaration_bodies`, `resolver.rs` `CompileTime::resolve_function_body`); `compiler/omega-analyzer/src/analysis/items/bodies.rs` (`check_struct_body`/`check_enum_body`/`check_union_body`, `check_method_bodies`); `compiler/omega-analyzer/src/dead_code.rs` (the checked-tree usage walker); `docs/architecture/module-driver-and-linkage.md` (instantiation, `decl_id_owner`, standalone emission paragraphs); `docs/language/generics.md`.
- **Initial context boundary (8b):** the saved draft `phase8-option-result-draft.patch` at the repository root (untracked). It contains `runtime/core/option.omg`, `runtime/core/result.omg`, cases `t78*`, and docs edits, all already working before the revert. It also contains three Rust test edits made only to tolerate the eager emission.
- **Affected files/symbols (8a):**
  - **Owner instantiation:** `ensure_item_at` calls `ensure_item_body` for every instantiation key. For an aggregate instantiation (struct/enum/union `ItemKey` with generic args), the checked aggregate item must carry fields/variants and **no** member bodies. Non-instantiated owners (a package's own concrete types) keep eager member bodies: they are that package's exported definitions.
  - **Per-member body:** give each concrete member of an instantiated owner a method `ItemKey` (owner = the instantiation key). Its body is checked by the existing `check_method_body` path, under the owner substitution and bounds `check_aggregate_body` already builds, with the owner's `@suppress` applied as `check_method_bodies` does today. It is cached in `generic_instantiations` and emitted standalone with `method_owner`, the same as generic member instantiations. `decl_id_owner` must map each member identity (from `ItemQueries::method_identities`) to that member key.
  - **`CompileTime::resolve_function_body`:** finds an instance member body through its member key. It must not check the whole owner, and must not mark the member for emission: a member reached only by `comp` is not emitted.
  - **Reachability:** extend `dead_code`'s walker, which already visits every expression and place, to also collect referenced function `decl_id`s (`CheckedPlaceRoot::Variable { storage: Storage::Function, .. }`). Do not add a fourth hand-written checked-tree walker.
    - In the driver, run a worklist over emitted items: module bodies, global initializers, generic instantiations, conformance and primitive bodies. Every referenced id whose `decl_id_owner` is an unemitted instance member gets its body ensured, emitted, and scanned in turn.
    - Fold this into `drain_pending_declaration_bodies`' loop (or one loop with it), so new instantiations, conformance bodies and members all reach a fixpoint before the diagnostics barrier in `compile/mod.rs`.
- **Interfaces/invariants:**
  - Every function `decl_id` referenced by emitted code has a definition in this compilation's output or in the extern catalog. Instance members are never in the extern catalog (`collect_extern_functions` skips instantiations), so every referencing package emits its own weak copy.
  - Symbols, mangling and linkage are unchanged. A member's symbol is the same whether it is emitted standalone or inside its owner item.
  - Member signatures stay eager, so lookup, overloads, conformance checking and vtables are unchanged.
  - Observable rule change: an error in a member of a generic owner is reported only for instantiations whose member is used, as generic member templates already behave.
  - No new runtime cost, allocation, or panic-support requirement.
- **Out of scope:**
  - Lazy member *signatures*; `as_ptr`/`as_mut_ptr` (keep the `compiler-limitations.md` entry from the draft, updated to say only signatures are still eager).
  - Phase 1.
  - `primitive<T>` blocks and conformance bodies, which have their own monomorphization paths (`drain_pending_declaration_bodies`): do not change when they emit.
  - Generic instances reached only by `comp` (an existing `compiler-limitations.md` entry), beyond not emitting comp-only instance members.
  - Callback-taking `Option`/`Result` methods, and the variant-construction restriction (recorded only).
- **Risks/open questions (stop and escalate):**
  - Something other than a checked-body `Storage::Function` place refers to an instance member, for example a vtable slot, reflection data, or an extern/export catalog, so that the scan cannot see it.
  - Instance-member dead-code (`unused_field`, `never_constructed_variant`) or other warnings change in a root case for a reason other than "fewer unused bodies are checked".
  - A root case relied on an error inside an unused instance member being reported.
  - The three draft Rust test edits turn out to still be needed after 8a.

## Implementation Plan
1. **Phase 8a — on-demand instance members.**
   1. Analyzer: let `check_struct_body`/`check_enum_body`/`check_union_body` check an instantiated owner without its member bodies (for example, take the member bodies to check as an argument). Keep the concrete-owner path unchanged.
   2. Driver: give instance members their own method keys and `decl_id_owner` entries. Route their bodies through `check_method_body` and standalone emission with `method_owner`. Make `resolve_function_body` use the member key.
   3. Add the function-reference collection to the `dead_code` walker. Build the emission worklist inside (or beside) `drain_pending_declaration_bodies`, before the diagnostics barrier.
   4. Docs:
      - `module-driver-and-linkage.md`: instance members are emitted on reference, and how the worklist relates to the pending-declaration drain.
      - `generics.md`, next to "instantiation actually formed": a generic owner's member is instantiated when used, not when the owner type is.
      - `compiler-limitations.md`: no stale eager-body claim.
   5. Commit.
2. **Phase 8b — `Option`/`Result` API.**
   1. `git apply phase8-option-result-draft.patch`.
   2. Revert the draft's edits to `compiler/omega-codegen/tests/alignment.rs`, `compiler/omega-driver/tests/function_namespaces.rs` and `compiler/omega-driver/tests/naked_functions.rs`. 8a should make them unnecessary; see Risks.
   3. Re-record the `t78*` `expected.stderr` files if `option.omg`/`result.omg` line numbers moved.
   4. Update the draft's `compiler-limitations.md` entry so it describes eager *signatures* only.
   5. Delete the patch file, then commit.

## Testing
- **Every phase:** `cargo build`; `cargo test` (whole workspace); `just test-all`; `just check-platform` (the aarch64 `ld.lld` link check cannot run where `ld.lld` is absent; say so rather than counting it); `cargo fmt --check`; `git diff --check`.
- **Phase 8a:**
  - **Component** (`compiler/omega-driver/tests/`, next to `generic_methods.rs`):
    - An instantiated generic struct/enum whose unused member contains a `match` emits no body for that member, and no panic-handler reference.
    - A used member of the same instantiation is emitted, once.
    - A member called only from `comp` is evaluated but not emitted.
    - A member reached only through another member's body is emitted.
  - **Root conformance** `t80_unused_instance_members`: `struct Wrap<T>` with a member that does not type-check for `Wrap<bool>` (for example `self.v + self.v`); a program using only `Wrap<bool>`'s fields compiles and runs. `expected.stdout`.
  - **Root conformance** `t80b_used_instance_member_errors`: calling that member on `Wrap<bool>` reports the body error, with full `expected.stderr`.
  - **Spec trace:** the new `generics.md` sentence.
  - **Regression:** `t47*`, `t70*`, `t71*`, `t72*`, the generic-method and weak-linkage driver tests, `omgc/tests/aligned_allocation_glue.rs`, `RUNTIME_CHECK_CASES` at `-O0` and `-O3`.
- **Phase 8b:**
  - `t78_option_result_api` and `t78b`–`t78g` (one per panicking method: `expected.stdout`, `expected.stderr`, `expected.status` = 134).
  - `aligned_allocation_glue.rs`, `function_namespaces.rs`, `naked_functions.rs` and `alignment.rs` pass **unmodified**.
