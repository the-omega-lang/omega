# Semantic identity refactor (phased)

## Task Description
- **Deliverable:** one behavior-preserving refactor of the analyzer/driver identity plumbing, executed in seven ordered phases. Each phase leaves the workspace building with the full gate green, is committed on its own, and may be implemented in a fresh developer session against this plan. Phase 7 then delivers the two features that the old plumbing blocked.
- **Purpose:** retire the approved design-debt entries, which all describe the same root problem: semantic identity carried by names, untyped vectors, or shared mutable cells rather than by explicit identities.
  - *Module paths and item paths are the same untyped `Vec<Ident>`*.
  - *A static-spec parameter's synthesized generic uses a fabricated `$ParamN` name*.
  - *Nominal semantic identity is coupled to `Rc<RefCell<Resolved*Type>>` cells*.
  - *`ModuleResolver` is a broad semantic service facade*.
  - *`reveal` has no structural backstop*, plus the related false "unnecessary `reveal`" warning in type position.
  - *`Driver::compile(&mut self)` looks reusable*.
  - *`CodegenRequest::entry` outlives its phase*.
  - *Nested statement fields lose their own source site*.
  - *`HirFor::init` is plural*.
  - Then implement the deferred items: joint owner-and-method generic inference, and `spec S` parameter sugar on conformance methods.
- **Chosen direction:** identities first, then the interfaces that carry them, then API shape, then features. Each identity type is introduced exactly once, so later phases (notably the resolver split) write their signatures once against final types.
  1. **`ModulePath` / `ItemPath` value types** in `omega-hir`. Self-describing and cheap to clone; no global interner, because module identity crosses analyzer → MIR → mangling and a side table would have to travel with it.
  2. **Positional generic identity.** A generic parameter is identified by `GenericParamId { owner: HirId, index: u32 }`. Signatures, inference and substitution work on positions in one place; names resolve only *lexical* references inside bodies, through ordinary scoping. The index-based form the compiler already has (`generics/pattern.rs::TypePattern::Parameter(usize)`, used by overload templates) becomes the **single** inference engine. The name-based engine (`generics.rs::unify_generic_type` over raw syntax `Type`) is removed.
  3. **Interned nominal type IDs.** `ResolvedType::{Struct, Union, Enum, Spec}` carry a `NominalRef`: a `Copy` `NominalId` plus an immutable `Rc<NominalIdentity>` (declaration `HirId`, kind, name, `ModulePath`, generic args), so `Display`, `Hash`, `Eq` and mangling need no store. The late facts (fields, functions, layout, suppressions, marker flag, enum tag/header/variants, spec functions) live in a driver-owned `TypeStore` as **write-once, per-stage slots** (fields, then functions, matching today's two-stage fill in `analysis/items/mod.rs`). Reading an unfilled slot is an explicit state, not a `RefCell` borrow panic.
  4. **Capability traits for `ModuleResolver`.** The trait becomes a supertrait of focused capability traits. Free helper modules that take the whole resolver today take only the capability they use, so focused tests can mock one capability.
  5. **Structural `reveal`.** A `reveal` prefix on a place is part of the place: HIR lowering records the reveal origins on `HirPlace`, and `analyze_place` activates them itself, so no caller can forget. Type resolution receives a reveal *authority* that records use, so a type-position `reveal` is marked used.
  6. **API and HIR/AST shape.**
     - `Driver::compile(self, ..)` consumes the driver.
     - `CodegenRequest::entry` is removed.
     - `HirFor::init: Option<Box<HirStmt>>`.
     - Nested AST statements keep `StatementNode`, so they keep their spans.
     - `ForInStmt` keeps its binding's span, and `SelfMode` keeps the span of its `self` token.
  7. **Features unblocked by phases 2–3.**
     - Joint inference of an omitted owner argument list and a generic member's own arguments from one call.
     - `spec S` parameter sugar on a conformance method satisfying a generic requirement.
- **Rejected alternatives:**
  - A global or thread-local path interner.
  - Replacing `Rc<RefCell<..>>` with `Rc<NominalCell>` holding `OnceCell` facts. Simpler, but it keeps pointer identity and gives no ID to key queries, caches or a future parallel/incremental store, which is what the debt entry asks for.
  - Alpha-renaming shadowed generic parameters. That is the `$ParamN` fabrication again.
  - Keeping both inference engines.
  - A mechanical split of `ModuleResolver` into many traits without narrowing any consumer.
  - One unphased change.

## Technical Details
- **Initial context boundary:**
  - Per phase, only the crates named in that phase.
  - Architecture docs: `docs/architecture/semantic-analysis.md` (phases 2, 4, 5, 7); `types-layout-and-const-eval.md` (phase 3); `module-driver-and-linkage.md` (phases 1, 4, 6); `parsing-and-hir.md` (phases 5, 6).
  - Language docs only in phase 7: `generics.md` § generic members and § `spec S` parameter sugar; `specs-and-conformance.md` conformance shape check.
  - `docs/language/` must not change in phases 1–6.
- **Measured footprint (to size each phase; not a checklist):**
  - `Vec<Ident>`/`&[Ident]`: analyzer 243, driver 200, MIR 35, parser 24.
  - `Rc<RefCell<Resolved…>>`: analyzer 62, driver 22.
  - `.borrow()` on cells: analyzer 168, driver 36, codegen 11, MIR 6. `.borrow_mut()`: analyzer 14, all in `analysis/items/mod.rs`.
  - `GenericSubstitution`: analyzer 44, driver 63.
  - `ModuleResolver`: 37 methods.
  - `Driver::compile` callers: `omgc/src/app.rs` plus 36 test files.

### Phase 1: `ModulePath` / `ItemPath`
- **Types:** add to `compiler/omega-hir/src/ids.rs`, next to `ModuleId`/`HirId` (HIR owns stable source identity):
  - `ModulePath`: a newtype over `Rc<[Ident]>`, with `Eq`/`Hash`/`Ord` by content, `Display` as `a::b`, `segments()`, `join(Ident) -> ItemPath`, `parent()`, `root()`.
  - `ItemPath { module: ModulePath, name: Ident }`.
- **Replace:**
  - the driver's `pub(crate) type ModulePath = Vec<Ident>` (`omega-driver/src/lib.rs:36`);
  - every module-identity `Vec<Ident>`/`&[Ident]` in the analyzer, driver and MIR adapters: `Resolved*Type::module_path`, `ExternFunctionRef`/`ExternGlobalRef::module_path`, `ConformanceOwner::spec_module_path`, `TemplateBound::module_path`, `SpecPattern::module_path`, `ItemKey`'s scope, every `ModuleResolver` parameter documented as a module (`accessor_module_path`, …);
  - every "absolute item path" that is immediately `split_last()` into module + item, with `ItemPath`.
- **Keep:** source-syntax paths (`omega_parser::ast::Path`) and mangling vocabulary (`omega-mangle`, which stays independent; adapt at `omega-mir::mangle`).
- **Invariant:** a root's declared name versus its on-disk stem keeps its single translation point (`canonical_query_path` and the doc-commented cache-key rule). Do not create a second.
- **Done when:** no `Vec<Ident>`/`&[Ident]` in analyzer/driver/MIR signatures denotes a module or item identity. Remaining uses are syntax or lists of names.

### Phase 2: positional generic identity
- **Identity:** add `GenericParamId { owner: HirId, index: u32 }` (analyzer). Its owner is the declaring item, function or alias. A member's own parameters are indexed after its owner's, so one owner-plus-member signature has distinct positions even when names shadow.
- **Declarations:** introduce an analyzer-level generic parameter declaration (`id`, `name: Option<Ident>`, an `origin` such as written or `StaticSpecParameter { param_index }`, kind, default).
  - `normalize_static_spec_params` (`generics.rs:36`) produces an *unnamed* parameter with that origin, instead of `Ident(format!("$Param{index}"))` plus rewriting the parameter type to `Type::Named("$Param…")`.
  - The parameter's declared type then resolves to that position.
  - Diagnostics that name such a parameter print its bound (`spec Show`) and point at the parameter's span.
- **Substitution:** `GenericSubstitution` is keyed by `GenericParamId`. Lexical scopes bind a written parameter's *name* to its id (`Context::define_type` and its callers in `analysis/mod.rs` about lines 480 and 690), so a body's `T` resolves through the scope chain and shadowing stays lexical.
- **One inference engine:**
  - All generic signature inference goes through the positional pattern form (`overload_template` / `TypePattern` / `ArgumentPattern`, `generics/pattern.rs`, `analysis/calls/pattern.rs`).
  - Remove `unify_generic_type` and its raw-`Type` callers.
  - Replace `GenericOwnerFunctionSignature { params: Vec<Type>, return_type: Type }` and the driver's `rewrite_self` (`omega-driver/src/resolver.rs` about lines 946–995) with a positional owner-plus-member signature. `Self` becomes `Nominal(owner, [Parameter(0..n)])`, never a name.
  - `resolve_inferred_generic_args` returns positions, keeping the gap semantics from the previous batch.
- **Also covered:** bound checking and alias-application obligations (`check_alias_generic_bounds`) use ids.
- **Mangling is unchanged:** `TemplateDescriptor` already uses positions.
- **Done when:**
  - `grep '\$Param'` finds nothing outside tests asserting its absence;
  - no inference path matches parameters by `Ident`;
  - the `$ParamN` design-debt entry is removed.

### Phase 3: interned nominal type IDs
- **Store:** `TypeStore` (analyzer type, owned by the driver), keyed by the driver's instantiation key, so two instantiations are distinct exactly when their `ItemKey`s differ, as today.
- **Creation:** `TypeCells::{struct,enum,union}_cell` (`omega-driver/src/items/mod.rs` about lines 147–210) and spec creation (`items/resolution.rs` about line 680) become `TypeStore::intern(key, identity) -> NominalRef`.
- **Filling:** the staged fills in `analysis/items/mod.rs` (about lines 884–935, 1289–1307) become write-once slot sets. Writing a slot twice is an internal error. Reading an unfilled slot returns an explicit "incomplete" result that the existing `InProgress`/cycle machinery reports.
- **Access:**
  - The analyzer reads facts through the resolver (a capability in phase 4), receiving `Rc`'d fact records so no borrow is held across further resolver calls.
  - `CompiledProgram` carries the frozen store into MIR lowering. `CodegenRequest` carries it into codegen.
  - Codegen/MIR helpers that read facts today through `.borrow()` take `&TypeStore` explicitly, including `omega_analyzer::layout::type_alignment` and related layout entry points called from `omega-codegen/src/llvm/*`.
- **Synthetic types** in tests and helpers (`calls/spec.rs` about lines 856–871, `omega-mir/src/mangle.rs` about line 470) go through a test store.
- **Done when:** no `Rc<RefCell<Resolved…>>` and no cell `.borrow()`/`.borrow_mut()` remain, and the design-debt entry is removed.

### Phase 4: `ModuleResolver` capabilities
- **Partition** the 37 methods (verify each against its body) into:
  - **Provenance:** `macro_origin_module`, `module_source`, `macro_authorship`, `function_source`, `trusted_panic_message_field`, `fresh_synthetic_id`.
  - **Names:** `resolve_explicit_anchor`, `resolve_module_path`, `declared_item_visibility`, `resolve_import_alias`, `resolve_visible_alias`, `ambient_core_candidates`, `import_alias_names`, `is_item_visible`, `similar_item_name`.
  - **Items and generics:** `resolve_item`, `item_generic_params`, `complete_generic_args`, `generic_function_signature`, `generic_literal_signature`, the phase-2 owner signature, `generic_method_template`, `instantiate_generic_method`, `generic_method_candidates`, `conformance_generic_method_candidates`, plus the phase-3 type facts.
  - **Calls:** `resolve_overload_set`, `function_value_candidates`, `instantiate_overload`, `prepare_generic_call`, `method_overload_candidates`.
  - **Conformances:** `spec_declaration`, `primitive_methods`, `conformance_for`, `conformances_for_type`, `conformances_for_specs`.
  - **Compile-time:** `resolve_function_body`, `resolve_comp_value`.
- **Shape:** `ModuleResolver: Provenance + Names + Items + Calls + Conformances + CompileTime` with a blanket impl. The driver implements each capability, and `Analyzer` keeps `&mut dyn ModuleResolver`.
- **Narrowing:** every free function or helper module that takes `&mut dyn ModuleResolver` today (for example `aliases::expand_type_alias`, `generics::normalize_static_spec_params`, `Context::resolve_type`, `comp_eval`) takes the narrowest capability it uses.
- **Test seam:** add one focused analyzer test that uses a mock of a single capability, as proof that the seam exists.
- **Done when:** no helper outside `Analyzer` takes the full facade without needing it, and the design-debt and known-issues entries are removed.

### Phase 5: structural `reveal`
- **HIR:** `omega-hir` lowering folds `reveal` prefixes that wrap a place-shaped operand into `HirPlace` (`reveal_origins: Vec<Origin>`, outermost first). A `reveal` around a non-place expression keeps `HirExpr::Reveal`.
- **Analyzer:**
  - `analyze_place` / `resolve_place_root` (`analysis/places/`) activate those frames and emit the unnecessary-reveal warning themselves.
  - Remove `with_reveal_operand` and `strip_reveal` from the call, assignment and address-of paths (7 call sites), keeping only the expression-level form for non-place operands.
- **Type position:** `resolve_type_or_error_in` (`analysis/mod.rs` about line 1465) passes a reveal authority that marks frames used (`&mut self.reveals` alongside `&mut *self.resolver`; disjoint fields), replacing the read-only `&|origin| reveals.allows(origin)`. This fixes the false "unnecessary `reveal`" warning on a type-position `reveal`.
- **Done when:** both reveal issue entries are removed, and a new place-consuming syntactic position cannot reach `analyze_place` without its reveal origins.

### Phase 6: API and HIR/AST shape
- **Driver:**
  - `Driver::compile(&mut self, entry)` (`omega-driver/src/compile/mod.rs:68`) becomes `compile(self, entry)`. Update `omgc/src/app.rs` and the test callers.
  - The target is already passed once (it lives in `CompilerDefinitions`), so the debt entry's "target twice" sentence is stale; drop it with the entry.
- **Codegen:** remove `CodegenRequest::entry` (`omega-codegen/src/request.rs`) and its construction in `omgc/src/app.rs` (about line 157). MIR keeps consuming the entry identity.
- **HIR:** `HirFor::init: Vec<HirStmt>` (`omega-hir/src/hir.rs` about line 466) becomes `Option<Box<HirStmt>>`. Consumers: `runtime_checks.rs:151`, `dead_code.rs:81`, `comp_eval.rs:1090`, `analysis/stmts.rs:396`, `omega-mir/src/lower/function/defer.rs:36`, `omega-hir/src/lower/statement.rs:72`.
- **AST:**
  - `ForStmt::init: Option<StatementNode>` and `DeferStmt::body: Box<StatementNode>` (`omega-parser/src/ast/statement.rs`);
  - a binding span on `ForInStmt`;
  - the `self` token's span next to `SelfMode` wherever a declaration stores it.
  - HIR lowering uses these spans instead of reconstructing them from the parent.
- **Done when:** the four entries are removed.

### Phase 7: features
- **Joint owner-and-method inference:**
  - `Owner::name(...)` / `m::Owner::self::name(...)` where `name` has its own generics and the owner list is omitted, or written with `_` holes, now infers both lists from one call over the phase-2 combined positional signature.
  - Explicit function arguments fix the member positions; explicit owner arguments fix the owner positions.
  - Remove the `function_generics.is_empty()` decline in `analysis/calls/generic.rs::resolve_generic_owner_function_call`, and the batch-3 rule that an owner list must be complete when a generic member's list is written.
  - Shadowing (`struct Owner<T> { name<T>(…) }`) must infer correctly. An owner parameter that only a shadowed name would mention is determined by its default or is reported "not determined" by its declared name.
- **`spec S` sugar on conformance methods:** the conformance shape check compares positional signatures, so `meet Take for S { take(*self, x: spec Show) => i32 }` satisfies `spec Take { take<T: Show>(*self, x: T) => i32; }`, and the reverse.
- **Docs:**
  - `generics.md`: remove the "owner list must be complete" sentence added in batch 3, and state joint inference.
  - `specs-and-conformance.md`: state that sugar and explicit generic parameters are interchangeable in a conformance.
  - `docs/issues/`: remove both entries.

- **Interfaces/invariants (all phases):**
  - Phases 1–6 change no observable language behavior. Every conformance `expected.*` file stays byte-identical, except where a removed bug's `expected.stderr` intentionally changes (phase 5's reveal warning). Each such change is justified in the commit.
  - No ABI, layout or mangled-symbol change in any phase. The full gate and `just check-platform` guard this.
  - Determinism: the `TypeStore` intern order, capability iteration and any map keyed by new ids keep deterministic ordering (`IndexMap` where order is observable).
  - Crate dependency direction is unchanged: `omega-mangle` stays standalone; codegen gains only a read-only `TypeStore` input.
- **Out of scope:**
  - Incremental or parallel compilation itself.
  - Macro-expansion fold/visitor design.
  - Any new language feature beyond phase 7.
  - Performance work beyond what falls out (no benchmarks required).
- **Risks/open questions (stop and escalate):**
  - Phase 2: if some generic body analysis genuinely needs to resolve a synthesized parameter by name (beyond the parameter's own declared type), stop and report the site.
  - Phase 3: if a fact is read before its stage is written along a path the cycle machinery does not cover (a genuine ordering dependency, not a convenience), stop and report it rather than reordering stages ad hoc.
  - Phase 3: if threading `&TypeStore` into codegen requires codegen to make a layout/ABI decision it does not make today, stop.
  - Phase 5: if a `reveal` wraps a place in a position where HIR cannot tell it is place-shaped without semantics, keep `HirExpr::Reveal` for that shape and report it.

## Implementation Plan
1. **Phase 1:**
   - Add `ModulePath`/`ItemPath` and convert the driver's internal alias first.
   - Then convert the analyzer's resolved type records and checked references.
   - Then `ModuleResolver` parameters, the MIR adapters, the docs (`module-driver-and-linkage.md` if it names the representation), and the issue entries.
   - Commit.
2. **Phase 2:**
   - Add `GenericParamId` and the declaration record, and convert `GenericSubstitution`.
   - Move owner-function inference onto positional patterns, then remove `unify_generic_type`.
   - Replace `$ParamN`.
   - Update `semantic-analysis.md` § generics/inference and the issue entries.
   - Commit.
3. **Phase 3:**
   - Introduce `TypeStore`/`NominalRef` alongside the cells.
   - Convert creation sites, then fill sites, then analyzer readers, then driver readers, then MIR/codegen readers.
   - Remove the cells.
   - Update `types-layout-and-const-eval.md` (nominal identity and completion stages) and the issue entries.
   - Commit.
4. **Phase 4:** capability traits, narrowing of the helper modules, the mock-capability test, `semantic-analysis.md`/`module-driver-and-linkage.md` resolver sections, issue entries. Commit.
5. **Phase 5:** HIR place reveal origins, analyzer activation, the type-position authority, `parsing-and-hir.md`/`semantic-analysis.md` reveal paragraphs, issue entries. Commit.
6. **Phase 6:** `compile(self)`, the `CodegenRequest::entry` removal, `HirFor::init`, AST spans, `parsing-and-hir.md`, issue entries. Commit.
7. **Phase 7:** joint inference, sugar conformance matching, language docs, issue entries. Commit.

## Testing
- **Every phase:**
  - `cargo build`; `cargo test` (whole workspace); `just test-all`; `just check-platform`; `cargo fmt --check`; `git diff --check`.
  - Run artifact rebuilds sequentially before the runner.
  - A phase is not done with any failing or newly skipped test.
- **Phase-specific tests:**
  - **Phase 1:** `omega-hir` unit tests for `ModulePath`/`ItemPath` (equality by content, display, join/parent). No new conformance cases are expected, since the behavior is unchanged.
  - **Phase 2:**
    - analyzer unit tests that owner-plus-member signatures keep distinct positions under shadowing;
    - that a static-spec parameter's diagnostics print its bound, never `$Param`;
    - the existing `compiler/omega-driver/tests/aliases.rs` `$Param` absence assertion keeps passing.
  - **Phase 3:**
    - analyzer/driver tests that two instantiations of one generic are distinct ids, and that one instantiation reached twice is one id;
    - that reading an unfilled stage is reported, not panicked;
    - a recursive type (`struct Node<T> { next: *Node<T>; }`) still resolves.
  - **Phase 4:** the mock-capability analyzer test.
  - **Phase 5:**
    - root case `t69_reveal_type_position`: a `reveal` on a cast target, an annotation and a generic argument naming a hidden type compiles with an empty `expected.compiler.stderr`;
    - and a truly unnecessary type-position `reveal` still warns, with full `expected.compiler.stderr`.
  - **Phase 6:**
    - an `omega-driver` test that `compile` consumes the driver (compile-time API shape; a doc-test or a usage test);
    - an HIR test that a `for` initializer and a `defer` body keep their own spans in a diagnostic (pick an existing diagnostic on such a statement).
  - **Phase 7:**
    - `t70_joint_owner_method_inference`: `Owner::name(7i64, 2u8)` where `name<B>` is generic; a module-qualified owner; a shadowing owner/member pair; `_` holes in the owner list with an explicit member list. Prints the values.
    - `t70b_joint_inference_errors`: an undetermined shadowed owner parameter; full `expected.stderr`.
    - `t71_static_spec_conformance`: the sugar and explicit spellings satisfying each other in both directions; `expected.stdout`.
- **Specification trace (phase 7 only):** `generics.md` generic members (updated), `generics.md` § `spec S` parameter sugar, `specs-and-conformance.md` conformance shape check.
- **Regression focus:**
  - Phase 2: `t47*`, `t62`–`t68`, `t52_comp_conformance`.
  - Phase 3: `t51_reflection`, layout/alignment codegen tests (`omega-codegen/tests/alignment.rs`), `t61_same_named_specs`.
  - Phase 5: `t13*` reveal cases.
