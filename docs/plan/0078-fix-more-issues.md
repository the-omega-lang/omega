# Full spec identity in conformance-method and vtable symbols

## Task Description
- **Deliverable:** conformance-method and vtable linker symbols encode the spec's full identity (module path + name + spec arguments). Two same-named specs in different modules then never share a symbol, within one compilation or across separately compiled packages. Remove the known-issues entry.
- **Purpose:** today `meet a::P for S` and `meet b::P for S` with the same method signature both mangle to `<S>::P::self::f`.
  - Within one package this is rejected as a symbol collision ("two different items both produce the linker symbol ..."), which is wrong for a valid program.
  - Across packages, the extern reference and the definition can bind to the wrong spec's method or vtable.
  - User decision: same-named specs in different modules must never collide. Symbol changes are acceptable pre-1.0, with no compatibility aliases.
- **Chosen direction:** add one new path form to the standalone mangling grammar, a **conformance path** `<Target as Spec₁ + … + Specₙ>`. The target is encoded as a `MangleType`, and each spec is its full nominal path (module + name + generic args), exactly as `semantic::nominal_path` already builds it for spec-object types.
  - A conformance method nests `self`/method beneath a one-spec conformance path.
  - A vtable nests the `vtable` value beneath a conformance path listing the shape's members in canonical shape order.
  - Every other symbol keeps its exact bytes.
- **Rejected alternatives:**
  - Nesting the spec's module segments as extra `Nested` segments under the target (`<S>::pkg::m::P`). It is not a real path, demangles misleadingly, and relies on the accident that types have no nested namespaces.
  - Appending the target as an extra generic argument of the spec path. That is ambiguous with a spec that has one more parameter.
  - Reusing `MangleType::SpecObject` as the spec component. Spec-object mutability is irrelevant here, and a conformance method belongs to exactly one spec.
  - Computing the vtable symbol in codegen. Symbol construction stays in `omega-mir::mangle` (architecture invariant 3).

## Technical Details
- **Initial context boundary:** `compiler/omega-mangle`, `compiler/omega-mir/src/mangle.rs` + `mangle/semantic.rs`, plus the narrow producer sites listed below. Docs: `docs/architecture/symbol-mangling.md` only. No language semantics change, so `docs/language/` stays closed.
- **Affected files/symbols (verified):**
  - **`omega-mangle`:**
    - `src/symbol.rs` `ManglePath`: add a `Conformance` variant holding `Box<MangleType>` (target) and `Vec<ManglePath>` (specs, non-empty, in caller order; never re-sorted).
    - `src/grammar.rs`: one new tag. `b'm'` is unused; verify against every tag constant, including the template-only ones, before taking it. Grammar: `m <type> <path>+ E`, reusing `TAG_LIST_END`.
    - `src/encode.rs`, `src/decode.rs` (`ManglePath::Type` handling at about line 132 is the closest model), `src/display.rs` (render `<Target as a::b::P<Args> + c::Q>`).
    - Back-references must work through the new form on both sides, so a repeated target or spec path inside the same symbol uses `TAG_BACKREF` exactly as other paths do.
  - **`omega-mir/src/mangle.rs`:**
    - `conformance_method_symbol` (about line 123): replace `spec_name: &Ident, spec_args` with the spec's module path, name and args. Build `ManglePath::Conformance(target, [nominal_path(spec_module, spec_name, spec_args)])`, then the existing `associated_function_path` / `template_path` / `generic_path` steps unchanged.
    - Delete the "NOTE" comments that describe the bug.
    - `vtable_symbol` (about line 148): take the canonical `&ResolvedSpecShape` (or the members' cells plus args) instead of `&[(Ident, Vec<ResolvedGenericArg>)]`, build specs from each member's `spec.borrow().module_path/name` + `spec_args`, and nest `vtable`. Keep the existing contract that callers pass canonical shape order and nothing here re-sorts.
    - `extern_function_ref_symbol`: the `ExternFunctionKind::Conform` arm passes the new module path through.
    - Update the in-file tests (about lines 488, 567–580, 704) to the new signatures.
  - **`omega-analyzer/src/checked.rs`:**
    - `ExternFunctionKind::Conform` gains `spec_module_path: Vec<Ident>`.
    - `ConformanceOwner` already carries `spec_module_path` (about line 189); it is now actually used.
  - **Producers:**
    - `omega-driver/src/compile/output.rs::collect_extern_functions` (`ExternFunctionKind::Conform`, about line 131): fill `spec_module_path` from `entry.spec.borrow().module_path`.
    - `omega-mir/src/lower/item.rs` (about line 328): pass `owner.spec_module_path`.
    - `omega-codegen/src/llvm/vtable.rs::vtable_for`: pass the shape straight to `omega_mir::mangle::vtable_symbol`, and delete the lossy `(name, args)` tuple construction. Its symbol-keyed dedup stays as is.
    - Search for any other `ExternFunctionKind::Conform { .. }` constructor or destructure, e.g. `omega-driver/tests/conform.rs:805`, and update it.
  - **Docs:** `docs/architecture/symbol-mangling.md`:
    - Add the conformance path to "Symbol model".
    - Rewrite "Conformance method identity" and "Vtables" to describe full spec identity, and delete the paragraph pointing at the known issue.
    - Record in the "Overloads"-style migration note that this change renamed every normally mangled conformance method and vtable.
    - Remove the entry from `docs/issues/known-issues.md` § Conformance and specs.
- **Interfaces/invariants:**
  - The `omega-mangle` round-trip `decode(encode(s)) == s` holds for every symbol, including the new form with back-references.
  - Encoding is alphanumeric/underscore only.
  - Only conformance methods (definitions, extern references, generic/blanket instantiations, spec-default bodies instantiated for a conformance) and vtables change name.
  - Free functions, methods, primitive methods, globals, gap/glue symbols, typeinfo, variant prototypes and data blobs are byte-identical. Prove that with existing exact-symbol tests passing unchanged.
  - Definition and extern-reference sides use the same helper, so they cannot disagree.
  - Weak linkage for monomorphized conformance methods is unchanged.
  - `omega-mangle` stays independent of compiler crates.
- **Out of scope:**
  - Any change to conformance semantics, orphan rules, duplicate-conformance diagnostics or vtable slot order.
  - Spec-object type mangling, which already includes module paths.
  - Glue symbols.
  - Compatibility aliases for old names.
  - Moving vtable symbol selection out of codegen into MIR data. That would be nice, but it is a separate representation change; note it in `docs/issues/design-debt.md` only if the review agrees it is real debt.
- **Risks/open questions (stop and escalate):**
  - If any analyzer- or driver-side identity (a conformance registry, dedup or vtable cache key) also keys on spec *name* alone, stop and report: that would be a semantic bug beyond symbols. The positive conformance case below exposes it if it exists.
  - If the decoder's back-reference scheme cannot represent a type followed by a path list without a grammar change beyond the one tag, stop and report.

## Implementation Plan
1. **`omega-mangle`:** add the variant, the tag, encode/decode/display, and round-trip tests: a primitive target, a nominal generic target, generic spec args, a multi-spec vtable shape, and back-references to a repeated target/spec. Add a demangle display assertion. Run `cargo test -p omega-mangle`.
2. **`omega-mir::mangle`:** new `conformance_method_symbol` / `vtable_symbol` signatures using the conformance path, and `extern_function_ref_symbol`. Update the in-file tests. Add a test that `a::P` and `b::P` conformance symbols differ for the same target and signature, and that two shapes differing only in a member's module differ.
3. **`ExternFunctionKind::Conform.spec_module_path`:** add the field and fill it in the driver; pass `ConformanceOwner.spec_module_path` in lowering; pass the shape in `vtable_for`. Build the workspace.
4. **Tests:** add the conformance and separate-compilation tests below, rebuild the runtime (`just build-runtime`, since `core`/`std` conformance symbols change), and run the focused cases.
5. **Docs:** update `symbol-mangling.md`, and remove the known-issues entry.

## Testing
- **Root conformance (new):** `tests/t57_same_named_specs/`, a package with modules `a` and `b`, each declaring `exposed spec P { f(*self) => i32; }`.
  - The root declares `struct S`, `meet a::P for S { ... 1 }`, `meet b::P for S { ... 2 }`.
  - It calls each through a static-spec call (`<S : a::P>::f`-style or a bounded generic), and through `*spec a::P` / `*spec b::P` objects.
  - It prints which implementation ran (`expected.stdout` shows `1` and `2` from both paths).
  - Also include a conjunction object over `a::P + b::P` if the language permits calling disambiguated members, checking `docs/language/specs-and-conformance.md`. Otherwise skip that part rather than inventing syntax.
  - **Specification trace:** distinct specs are distinct items (`modules-and-imports.md` identity, `specs-and-conformance.md` conformance per spec). Today the case fails with the symbol-collision error.
- **Separate compilation (new):** `compiler/omgc/tests/conformance_identity_linkage.rs`, modelled on `extern_global_linkage.rs`.
  - Package `lib` has modules `x` and `y`, each exposing `spec P` with the same method signature, an exposed `struct S`, and `meet x::P for S` / `meet y::P for S` returning different values.
  - The consumer package calls both conformance methods (extern references) and coerces `&s` to `*spec x::P` and `*spec y::P` (vtables emitted in the consumer).
  - Compile separately, link, run, and assert the output.
  - Also assert with `nm` (if available, mirroring the existing `tool_available` pattern) that the two methods and two vtables have distinct symbols and that the consumer's undefined references match the provider's definitions.
- **Regression coverage:**
  - `cargo test -p omega-mangle -p omega-mir -p omega-driver -p omega-codegen -p omgc` (existing exact-symbol tests for non-conformance symbols must pass unchanged).
  - `t*` spec/conformance cases (`grep -l meet tests/*/*.omg`).
  - `t51_reflection`, `t52_comp_conformance`, `just check-platform` (its shared-library check names exported symbols).
- **Commands:** `cargo build`; `just build-runtime`; focused `./bin/test-runner t57_same_named_specs <spec cases>`; `just test-all`; `just check-platform`; `cargo fmt --check`; `git diff --check`.
