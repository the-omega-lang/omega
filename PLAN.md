# Reflection: `typeinfo<T>` operator + reflection issue fixes

## Task Description
- **Deliverable:**
  1. `typeinfo<T>` becomes a compile-time **operator** in the `sizeof<T>`/`alignof<T>` family. It replaces the compiler-implemented function `core::reflection::typeinfo<T>()`, which is removed.
  2. Every tracked reflection issue in `docs/issues/known-issues.md` § Reflection is fixed, and so is the `comp` conformance-method limitation that section cites.
- **Purpose:**
  - `typeinfo` is constant data the compiler reflects; no code runs. A function is the wrong category: it emits a real weak function per `T` and must be imported. `sizeof` is the precedent.
  - The open issues are a compiler crash (P1), unsound identity for function and spec-object types (P1), and `comp`/runtime disagreement on names (P2).
- **Chosen direction:**
  - **Operator plumbing:** `typeinfo` is a contextual keyword committed only before `<`, exactly like `alignof` (`omega-parser/src/parser/expression.rs:661`). It flows through AST and HIR like `Alignof`. The analyzer checks it straight to `CheckedExpr::Const(ConstValue::Reflected(..))` typed `*core::reflection::TypeInfo`. The value doesn't depend on the target, so no new `CheckedExpr`/MIR variant is needed: the existing `Reflected` const path through `comp`, MIR and codegen stays.
  - **Result type:** the analyzer finds `core::reflection::TypeInfo` through the existing `ModuleResolver::resolve_item` with that absolute path. The path is held once, as a constant in `omega-analyzer/src/reflection.rs`. If it can't be resolved, that is a diagnostic, not a panic.
  - **Table type carried in the reference:** `Reflected::TypeInfo` carries the table's resolved `TypeInfo` type as well as the described type. Dereferencing and codegen use that carried type and never the pointer's current pointee. This fixes the cast crash.
  - **Identity by components only:** `Function` and `SpecObject` get real payloads, so every kind has an identity made of components and `Eq` never compares `name`. `name` becomes presentation only, but it is deterministic: unnamed types render from their identity components, with function parameter descriptors removed.
  - **Conformance methods under `comp`:** the driver can check them on demand, so `Eq for TypeInfo` works under `comp`. That makes `Eq` the single identity test everywhere.
- **Rejected alternatives:**
  - *Keeping the function form:* it is the wrong category, has a hidden per-`T` instance, and needs an import.
  - *Ambient-name lookup of `TypeInfo`:* a user's own `TypeInfo` would shadow it. The absolute path cannot be shadowed.
  - *A new `CheckedExpr::Typeinfo` that is lowered later:* `sizeof` needs a deferred node because its value depends on the target, but `typeinfo` resolves to a symbolic constant at check time.
  - *Identity by display spelling for opaque kinds:* that is exactly the P1 bug.
  - *Keeping the `comp`-only meaning of `==` as type identity:* see the interfaces section.

## Technical Details
- **Initial context boundary:**
  - `omega-parser`: `parser/expression.rs`, `parser/contextual.rs`, `ast/expression.rs`, `macros/expander.rs`.
  - `omega-hir`: `hir.rs`, `lower/expression.rs`.
  - `omega-analyzer`: `analysis/exprs/mod.rs`, `reflection.rs`, `resolved_type.rs` (`Reflected`), `compiler_functions.rs`, `comp_eval.rs`, `analysis/calls/mod.rs`, `analysis/patterns.rs`.
  - `omega-driver`: `resolver.rs::resolve_function_body`, `compile/bodies.rs::check_conformance_bodies`, `bodies.rs::ensure_item_body`.
  - `omega-codegen`: `src/llvm/reflection.rs`.
  - `runtime/core/reflection.omg`; `docs/language/reflection.md`.
  - Do not reopen MIR or mangling unless a contract below changes.
- **Affected files/symbols:**
  - **Parser:** add a `TYPEINFO` constant in `contextual.rs` and the `typeinfo` arm next to `alignof` in `parser/expression.rs`, plus `Expression::Typeinfo(Box<TypeinfoExpr>)` in `ast/expression.rs` and its arm in `macros/expander.rs`. Update `tests/contextual_keywords.rs`.
  - **HIR:** `HirExpr::Typeinfo(Type)` and its lowering arm.
  - **Analyzer expression:** in `analysis/exprs/mod.rs` next to `HirExpr::Alignof`, resolve the target type, resolve `TypeInfo` through `reflection::TYPE_INFO_PATH` + `resolve_item(.., ResolveItemOptions::DIRECT)`, and produce a node of type `*TypeInfo` (immutable) whose kind is `CheckedExpr::Const(reflection::type_info_ref(&target, &info_type))`. A failed lookup gets a new `AnalysisErrorKind` ("`typeinfo` requires `core::reflection::TypeInfo`") with its render arm.
  - **Remove the function form:**
    - Delete `CompilerFunction::TypeInfo` and its classifier, body and test arms (`compiler_functions.rs`, `compiler_functions/tests.rs`, `analysis/items/bodies.rs`).
    - Delete the declaration in `reflection.omg`.
    - Keep the per-function module structure in `compiler_functions.rs` only if it still reads naturally with volatile alone. Otherwise return it to its single-module form. Either way, the `MalformedCompilerFunction` message must stay correct.
    - Revert the `t48c_function_without_body/expected.stderr` wording to whatever the bodyless rule says once only `core::volatile` qualifies.
  - **`Reflected`** (`resolved_type.rs:666`) becomes `TypeInfo { described: ResolvedType, table: ResolvedType }`. `VariantPrototype` already carries its enum type.
    - `type_info_ref(described, table)` widens refined types as now.
    - The builder passes `self.info_type` into every nested ref it creates.
    - Equality stays derived. `table` is the same for every reference.
  - **Dereferencing** (`reflection::deref` and its three callers: `analysis/calls/mod.rs`, `analysis/patterns.rs`, `comp_eval.rs`):
    - Build from the carried `table`.
    - If the pointer's pointee is not that type (after `widened()`), fail with the unsupported reason "reading reflection data through a pointer of another type". That is a diagnostic, never a panic.
  - **Codegen** (`llvm/reflection.rs::reflected_global` / `typeinfo_global`): build from the carried `table` and ignore the pointee. A `*u8`-typed `Reflected` constant must still emit the table and a pointer to it.
  - **New payloads** (`reflection.omg`, built in `reflection.rs`):
    ```omega
    exposed enum CallingConvention { Omega, C, SysV64; }
    exposed struct SpecInfo { exposed name: *str; exposed path: *[]*str; exposed generic_args: *[]GenericArg; }
    # in TypeKind:
    Function { exposed params: *[]*TypeInfo; exposed ret: *TypeInfo;
               exposed variadic: bool; exposed convention: CallingConvention; },
    SpecObject { exposed specs: *[]SpecInfo; exposed mutable: bool; };
    ```
    - `params` come from `ResolvedFunctionType::param_types()`, so descriptors are dropped. `convention` maps `resolved_type::CallingConvention`.
    - `specs` follow `ResolvedSpecShape.members` in their existing canonical order, each member's spec declaration name, `module_path` and `spec_args`.
    - `SpecInfo` keeps its name: later reflection of spec declarations extends it additively.
    - Check that function-*value* types never carry `self_mode` (the receiver is an explicit parameter for `Type::self::name`, per `functions.md`). If one can, escalate rather than guess.
  - **`Eq for TypeInfo`** (`reflection.omg`):
    - `Function` compares `params` element-wise, `ret`, `variadic` and `convention`.
    - `SpecObject` compares `mutable` and `specs` element-wise by `path`, `name` and `generic_args`.
    - Delete `same_name`. `name` is compared only as part of a named type's or spec's identity.
  - **Canonical names** (`reflection.rs::naming`): an unnamed type's `name` renders from a copy of the type with every function parameter descriptor cleared at every depth (pointer to function, array of function, and so on). The rule: equal types give equal names, both under `comp` and in emitted initializers.
  - **Conformance methods under `comp`** (`omega-driver`): `resolve_function_body` returns `None` today for a `__conform_N` owner, because `ensure_item_body` cannot check conformance bodies.
    - Add a memoized, cycle-guarded on-demand check of one conformance method body by method `decl_id`. The guard is the same in-progress/finished discipline as `items.begin_body`/`finish_body`.
    - Factor it out of the per-method part of `check_conformance_bodies`, so the sweep and the query share one body check and one cache. That covers both concrete methods and the `pending` spec-default methods (e.g. `Eq::not_equals`).
    - The sweep then reuses cached bodies instead of re-checking them.
- **Interfaces/invariants:**
  - `typeinfo<T>` emits nothing by itself. A table exists at run time only when a runtime-reachable `Reflected` constant references it. There are no `typeinfo` function instances anymore.
  - `Eq for TypeInfo` is the single identity test, under `comp` and at run time. `==` on two `*TypeInfo` compares addresses. Under `comp` that coincides with type identity because a compilation has exactly one table per type. Keep the existing `Reflected` `==` arm in `comp_eval.rs` and document it as address equality, not as a special identity rule.
  - The one builder in `reflection.rs` stays the single source for `comp` and codegen.
  - No function addresses in tables (function *types* only). Listing which specs a type implements stays out of scope.
  - The nested `*TypeInfo` / `TypeKind` layout above is the contract. It is a breaking change, which is fine before 1.0.
- **Out of scope:**
  - The general "generic instances reached only by `comp` are still emitted" limitation. Its reflection consequence disappears with the function form, so delete only that sentence from `compiler-limitations.md`.
  - Field access by `comp` index, user annotations, `Any`/`Typed`, and connecting `==` to `Eq`.
- **Risks/open questions (escalate):**
  - The conformance on-demand check creates a driver cycle that the existing guard cannot report (for example a conformance whose own signature resolution needs a `comp` call into itself).
  - `resolve_item` for an absolute `core` path is unavailable from some accessor contexts, such as a package built without `core`. Report how the compiler handles a missing `core` today before choosing a behavior.

## Implementation Plan
1. **Operator front end:** parser keyword and AST/HIR node, then the analyzer arm producing the `Reflected` constant with the resolved `TypeInfo` type. Add the new `Reflected::TypeInfo { described, table }` shape, and update `type_info_ref`, the builder, `deref` and codegen to use `table` (this also fixes the cast crash). Keep the old function temporarily so the tree builds.
2. **Remove the function form:** classifier arm, body builder, declaration, tests. Switch `tests/t51_*`, `compiler-functions` tests and `omega-codegen/tests/reflection.rs` to `typeinfo<T>`.
3. **`Function`/`SpecObject` payloads, `SpecInfo`, `CallingConvention`;** rewrite `Eq` and delete `same_name`.
4. **Canonical descriptor-free naming** for unnamed types.
5. **Driver:** on-demand conformance method bodies for `resolve_function_body`, shared with the sweep.
6. **Docs:**
   - `docs/language/reflection.md`: operator form and grammar, new payloads, identity rule for every kind, `Eq` under `comp`, `==` as address equality. Delete the "rejected expression form" bullet and add a rejected "compiler-implemented function" bullet with the category rationale.
   - `lexical-structure.md`: add `typeinfo` to the contextual keyword list.
   - `grammar.md`: add `typeinfo` next to `sizeof`/`alignof`.
   - `annotations-and-sizeof.md`: one cross-reference sentence.
   - Remove every `typeinfo` mention from `volatile.md` and `functions.md`, and make `compile-time-evaluation.md` say conformance methods are evaluable.
   - `docs/architecture/`: update `mir-and-codegen.md` and `types-layout-and-const-eval.md`, where the `typeinfo` function and `Reflected` are described. Update `runtime-and-platform.md` so `core::reflection` is no longer compiler-implemented declarations, only data types the operator names.
   - `docs/guide/quick-reference.md` and `docs/guide/core-library.md`: switch to the operator form.
   - `docs/issues/`: delete the three Reflection entries and the "Reflection" section intro. Delete the conformance-under-`comp` entry. Delete the reflection sentence in the comp-only-instances entry.

## Testing
- **Rust (component):**
  - Parser: `typeinfo<T>` parses only before `<`, and `typeinfo` stays a usable identifier elsewhere (`contextual_keywords.rs`).
  - `reflection/tests.rs`: `Function`/`SpecObject` payloads, descriptor-free names (`(x: i32) => void` and `(y: i32) => void` give the same `name`), the carried table type surviving a cast.
  - `omega-codegen/tests/reflection.rs`: a `*u8`-typed `Reflected` constant emits the table. Two fields with descriptor-differing function types share one table with one initializer. No `typeinfo` symbol or function exists when reflection is unused.
- **Conformance `tests/t51_reflection/`** (proves `docs/language/reflection.md`), updated to the operator form, plus:
  - `Eq` under `comp`: `comp SAME := Eq::equals(*typeinfo<Pair<i32>>, *typeinfo<Pair<i32>>);` and a differing pair.
  - Function types from distinct `a::T` / `b::T` and spec objects of distinct `a::S` / `b::S` compare unequal through runtime `Eq` and `comp` `Eq` (the previous P1 reproducer).
  - The P2 reproducer printing both fields' `type.name` under `comp` and at runtime, identical.
  - Keep the two-source-file case (`other.omg`) to cover cross-unit WeakODR initializer agreement.
- **Negative `tests/t51b_reflection_errors/`**, with an exact `expected.stderr`:
  - `comp X := *<*u8>typeinfo<i32>;` reports the "pointer of another type" unsupported diagnostic, where it used to crash.
  - `comp P := <*u8>typeinfo<i32>; main() => void { p := P; }` builds (no crash). Put it in `t51_reflection` if it has observable output.
  - Keep the prototype-read case.
- **Conformance for the driver fix:** a non-reflection case, `comp X := Eq::equals("a", "a");`, in the closest existing `comp` case (`t34_comp_generics` or a new `t52_comp_conformance`). Also a `comp` call to a spec-default method (`not_equals`).
- **Regression:** `t48_volatile`, `t48b_volatile_errors`, `t48c_function_without_body`, `t50_comp_panic`, `t15_annotations_and_sizeof`, then `just test-all`.
