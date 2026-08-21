# Enforce the `main` entry-point signature

## Task Description

- **Deliverable:** the root module's `main` function is accepted only with signature `main() => void` or `main() => never` (no parameters, no generics). Any other root-module `main` signature is a compile error with a clear diagnostic. Functions named `main` in non-root modules are unaffected. Additionally, the Omega-level entry point is decoupled from the native platform `main` C symbol: Omega's root `main` is emitted as a fixed internal symbol `_omg_main`, and each platform's `plat` implementation is responsible for providing the actual native entry point that calls it.
- **Purpose:** command-line argument passing and "return value becomes process exit code" are platform-dependent assumptions that do not hold on embedded/freestanding targets. Restricting `main` to a fixed, argument-less, non-value-returning signature keeps the entry point a portable notion. (Follow-up capabilities -- a command-line-arguments `gap` and an explicit exit-code `plat` function -- are intentionally not part of this task.)
- **Chosen direction:**
  1. Validate the signature once, in `omega-driver`, at the point that already knows both the entry module path and each module's resolved function signatures (`Driver::collect_signatures`). This reuses already-computed `ResolvedFunctionType`s instead of re-deriving signature semantics elsewhere. **(Already implemented -- see "Status" below.)**
  2. Stop emitting the root-module `main` as the literal native `main` symbol. Emit it as a fixed, forced symbol `_omg_main` instead (`omega-mir`'s existing `is_root_main` special case, renamed). Each `plat` implementation that wants to produce a runnable native program provides its own adapter that owns the literal platform `main`/`_start` entry point and calls `_omg_main`.
- **Rejected alternatives:**
  - Validating inside `omega-mir::lower::item::is_root_main` or `omega-analyzer::collect_function_signature` -- rejected for the reasons already recorded in "Status" below (still valid).
  - Keeping the root `main` mapped directly to the literal C `main` symbol -- **rejected, this was the original design and it is unsound.** Discovered mid-implementation: the default `just test-all` runtime links against the real platform libc CRT (musl in this sandbox, glibc elsewhere) via plain `cc`, not `runtime/shims/x86_64-unknown-linux.S`'s own `_start` (that shim is a separate freestanding config not exercised by the default build). The libc CRT calls the native `main` symbol as C's `int main(void)` and uses `%eax` as the process exit status. `main() => i32 { ...; return 0; }` happened to leave 0 there by coincidence; a `void`/`never` `main` leaves `%eax` with undefined leftover garbage, so every linked program now exits with an undefined nonzero status. Verified empirically (`objdump`/direct run of a compiled `tests/t00_hello_world` showed exit status 1). Do not reintroduce this coupling.
  - Routing `_omg_main` through the ordinary mangling algorithm instead of a forced literal -- rejected because `compiler/omega-mir/src/mangle/semantic.rs`'s `signature()`/`mangle_type()` encodes the return type into the mangled symbol, and `ResolvedType::Void`/`ResolvedType::Never` map to distinct `MangleType::Void`/`MangleType::Never` variants. `main() => void` and `main() => never` would therefore mangle to two *different* strings. The `plat` adapter (below) needs one fixed name to `extern`-declare and call, written once, regardless of which of the two allowed signatures a given program's `main` uses -- so this must stay a forced literal, exactly like the existing `is_root_main` special case already is (just renamed), not routed through the general mangling algorithm.

## Status

Implemented and building cleanly (`cargo build --workspace` clean):

- `compiler/omega-analyzer/src/error/kind.rs`: `AnalysisErrorKind::InvalidMainSignature` variant + `Display` arm (next to `ReturnTypeMismatch`).
- `compiler/omega-analyzer/src/error/render.rs`: matching render arm (label + note).
- `compiler/omega-driver/src/compile/signatures.rs`: `Driver::check_main_signature(&mut self, entry: &[Ident])`, called from `collect_signatures` after its per-module loop. Guards on the entry module actually being parsed/indexed (`self.modules.get(entry)` -- a namespace-only root directory, e.g. `runtime/core/`, was never parsed and has no `main`), then looks up `main` via `ModuleIndex::overloads`/`items`, skips generic `main`s (unreachable/never linked, out of scope), fetches the already-resolved `ResolvedFunctionType` (`ensure_overload_signature` for an overloaded `main`, `ensure_item` for a plain one), and validates `params.is_empty() && matches!(*return_type, ResolvedType::Void | ResolvedType::Never)`.
- `compiler/omega-driver/src/compile/mod.rs`: `collect_signatures` now takes `entry: &[Ident]`; call site updated.
- `docs/language/foreign-function-interface.md`: "Program entry point" section states the signature requirement. **Needs a further update for the `_omg_main` change -- see Implementation Plan step 4 below.**
- All 21 root `tests/t*/` conformance cases' root `main`s migrated from `main() => i32 { ...; return 0; }` to `main() => void { ... }`. `tests/t19_foreign_function_interface/helper.omg`'s `exposed main() => i32 { ... }` is a non-root-module function (reached via `helper::main()`) and was correctly left untouched.
- `runtime/shims/x86_64-unknown-linux.S`'s `_start` was edited from `call main; mov %rax, %rdi; call exit` to `call main; xor %edi, %edi; call exit` (stop trusting the return register). **Needs a further edit for the `_omg_main` rename -- see Implementation Plan step 3.**

Do not redo this work or its investigation. What follows is the remaining, previously-unplanned work.

## Technical Details

### Initial context boundary

- `compiler/omega-mir/src/lower/item.rs` (`is_root_main`, `free_function_symbol`).
- `runtime/plat/libc/libc.omg` (the default libc-backed `plat` implementation -- read in full, it is short).
- `runtime/shims/x86_64-unknown-linux.S` (freestanding `_start`).
- `docs/language/foreign-function-interface.md` ("Symbol naming" section for `@mangling(force = "...")` syntax/precedent; "Program entry point" section to update).
- `docs/guide/platform-glue.md` if it documents what a `plat` implementation is expected to provide (check before writing the adapter; do not assume its conventions).

### Affected files/symbols

1. `compiler/omega-mir/src/lower/item.rs`: in `free_function_symbol`, change the `is_root_main` arm's forced literal from `"main".to_owned()` to `"_omg_main".to_owned()` (~line 173). `is_root_main` itself (`path == entry && function.name.as_ref() == "main"`) is unchanged -- it still identifies the Omega-visible root-module function named `main`; only the emitted symbol string changes.
2. `runtime/plat/libc/libc.omg`: add an adapter function that owns the literal platform-facing `main` C symbol:
   ```
   internal extern _omg_main : () => void;

   @mangling(force = "main")
   platform_main() => i32 {
       _omg_main();
       return 0;
   }
   ```
   (Confirm the exact placement/style against the file's existing `internal extern` block and glue definitions; keep the Omega-side function name descriptive -- e.g. `platform_main`, not literally `main` -- so it reads clearly as a forced-symbol adapter rather than something that could be confused with `is_root_main`'s own special case, which only ever fires for a function actually named `main` in an entry module and would not fire here regardless since `ManglingMode::Forced` is matched before the `is_root_main` arm in `free_function_symbol`.) Calling a `void`-declared extern whose real linked definition is actually `never`-returning needs no special handling: confirmed by reading `omega-analyzer::analysis::items::analyze_extern_decl` that an `extern` declaration is type-resolved and validated purely locally (parameter/return-type well-formedness, the aggregate-by-value FFI restriction) with no cross-module/cross-package lookup of any "real" definition -- extern-to-definition linkage is symbol-based at the native linker, not type-checked by Omega. This mirrors the language's own divergence-compatibility rule (a diverging call is valid in any expected-type context) and needs no code change to support.
3. `runtime/shims/x86_64-unknown-linux.S`: change `_start`'s `call main` to `call _omg_main` (this shim owns the raw entry point directly -- it has no reason to go through a C-`main`-shaped adapter the way the libc `plat` needs to). The already-applied `xor %edi, %edi; call exit` stays as-is.
4. `docs/language/foreign-function-interface.md`, "Program entry point" section: update to state that a root-module `main` is compiled to a fixed internal symbol (name it, e.g. "the root `main` is not itself the platform's native entry-point symbol; each `plat` implementation is responsible for providing that and invoking Omega's entry point"). Keep this description at the level of an implementation contract useful to a `plat` author, not full mangling internals (those belong in `docs/architecture/symbol-mangling.md` if that document already covers root-`main` specially -- check it; update only if it already describes the old `"main"` special case and would now be stale). Do not invent a new public language-level name for `_omg_main` unless `docs/guide/platform-glue.md` already establishes a naming convention for this kind of internal contract -- check it first.

### Interfaces/invariants

- `_omg_main` is a compiler-owned, fixed internal linkage contract, not a user-facing language feature -- there is no Omega source syntax that lets a user opt into or spell this name; it exists purely as the boundary between `omega-mir`'s entry-point lowering and a `plat` implementation's adapter.
- Only the entry module's function literally named `main` becomes `_omg_main` (unchanged `is_root_main` predicate). A `main` in any other module remains an ordinary mangled function.
- A `plat` implementation that wants to produce a runnable native binary must supply exactly one adapter that resolves to the platform's real entry-point symbol (`main` for libc-linked targets, `_start` for the freestanding shim) and calls `_omg_main`. A `plat` implementation that provides no such adapter still links and works as a library-mode dependency; only producing a runnable executable requires it (consistent with "no language-level library/program mode").
- One fact, one owner: the entry-point *symbol identity* decision stays solely in `omega-mir`'s `free_function_symbol`/`is_root_main`; the entry-point *native ABI adapter* decision stays solely in each `plat` implementation. Neither the analyzer/driver signature check nor the freestanding shim need to know about the other's mechanism.

### Out of scope

- No command-line-argument `gap`.
- No process-exit-code `plat` function or any user-facing way to choose a non-zero/non-default exit status (the libc adapter always returns a fixed 0; that is required baseline behavior for "falling off the end of `main` exits successfully", not the deferred user-facing feature).
- No handling of a generic `main` beyond leaving it exactly as unreachable as it already is today.
- No changes to `runtime/plat` implementations other than `libc` (there are none currently -- confirmed `runtime/plat/` contains only `libc/`).

### Risks/open questions

- If `docs/guide/platform-glue.md` documents a different/existing convention for how a `plat` implementation provides a native entry point, follow that convention instead of inventing the `@mangling(force = "main")` adapter shape described above -- read it before implementing step 2.
- None else identified that require stopping.

## Implementation Plan

1. `compiler/omega-mir/src/lower/item.rs`: change the `is_root_main` arm's forced literal to `"_omg_main"`. Build `omega-mir`/`omega-driver`/`omgc`.
2. `runtime/shims/x86_64-unknown-linux.S`: change `_start`'s `call main` to `call _omg_main`.
3. `runtime/plat/libc/libc.omg`: add the `internal extern _omg_main : () => void;` declaration and the `@mangling(force = "main")` adapter function, per `docs/guide/platform-glue.md` conventions if any exist.
4. `docs/language/foreign-function-interface.md`: update "Program entry point" per the Affected-files note above.
5. Rebuild `omgc` and the runtime objects (`just build-omgc build-runtime`, or `just test-all` which does both), and confirm the produced `target/plat.o` actually defines `main` and references `_omg_main` (`nm`/`objdump` spot check is reasonable here given this is a linkage-identity change, not just a normal conformance case).
6. Run the full existing conformance suite (`just test-all`) and confirm every case now both compiles *and* runs to a genuine exit status 0 (not just "didn't crash") -- this is the regression that motivated this revision, so do not treat a green compile as sufficient.

## Testing

- **New/changed cases (root `tests/`):**
  - `tests/t22_program_entry_point/` (or the next free number after `t20_operators`): positive case proving `main() => never` compiles, links through the real default libc `plat`, runs, and the process exits 0. Since `never` requires genuine divergence, declare a local `internal extern exit : (code: i32) => never;` (the linked-in `exit` symbol already exists via libc) and end `main` with `exit(0);` after a labeled `println$` call proving the body ran. This case's `expected.stdout` should assert the label ran; the test runner's own pass/fail already requires the process to exit successfully, which is precisely the property this revision fixes -- so this case doubles as the regression proof for the `_omg_main`/adapter mechanism, not just a `never`-signature conformance case.
  - A negative case with a root `main` taking a parameter, e.g. `main(argc: i32) => void { }` -- expect `expected.stderr` containing the `InvalidMainSignature` diagnostic text, no `expected.stdout`.
  - A negative case with a root `main` returning a non-`void`/`never` type, e.g. `main() => i32 { return 0; }` -- expect the same diagnostic.
  - Keep negative cases single-purpose per the project's expected-output convention (compile-failure only, matched via `expected.stderr`).
- **Specification trace:** `docs/language/foreign-function-interface.md`, "Program entry point" section (as updated in step 4).
- **Negative/diagnostic cases:** both negative cases above must assert the exact rendered diagnostic text via `expected.stderr`, not merely "compilation failed".
- **Regression coverage:** the entire existing root `tests/` suite is regression coverage for the `_omg_main` rename specifically, since it changes how *every* test package's `main` gets linked into a runnable executable -- a mistake here manifests as every case's process exiting nonzero (exactly the failure mode already observed once), not as a compile error, so a full `just test-all` run (not just compilation) is required before considering this done.
- **Commands/target coverage:** `./bin/test-runner t00_hello_world t22_program_entry_point <new-negative-cases>` for a focused pass once `omgc`/runtime objects are rebuilt, then `just test-all` for the full gate. Only one backend shim/plat exists for this target, so single-target (x86_64 Linux, libc `plat`) coverage is sufficient; the change is otherwise MIR-level symbol-naming, which both Cranelift and LLVM consume identically through the shared `CodegenRequest`/symbol contract -- no backend-specific behavior is introduced.
