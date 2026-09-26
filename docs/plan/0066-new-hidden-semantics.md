# `hidden` means "hidden to other modules", everywhere

## Task Description
- **Deliverable:** `hidden` has one meaning for every declaration kind: visible throughout the declaring module (= one source file), invisible elsewhere without `reveal`. Hidden fields, methods, and spec requirements follow the rule top-level items already use.
- **Purpose:** Remove the second, owner-scoped meaning of `hidden` for members. Today a hidden spec requirement is implementable but effectively uncallable, and a type's own module can't construct or read its hidden fields outside the type's methods. Encapsulation granularity becomes the module, as with Rust's private: to seal a type's fields completely, put the type alone in its module.
- **Chosen direction:**
  1. **One rule.** Member visibility uses the same `visibility_allows(visibility, declaring_module, accessor_module)` check as items. The owner-only branch and `current_owner` go away.
  2. **Declaring module of a member is recorded, not inferred.** `ResolvedMethod` gains a `declaring_module` field, set where the method is constructed:
     - an aggregate's inherent method → the aggregate's module (fields keep using the aggregate's `module_path`);
     - a spec-sourced method (conformance bodies and minted default bodies) → **the spec's** `module_path`, never the receiver type's;
     - a `primitive` block method → the `core` module that declares the block (`PrimitiveEntry.module`).
     The visibility check then never derives a module from the receiver type.
  3. **Defining is not using.** A `meet` anywhere the orphan rule allows may supply a body for a hidden requirement. Calling it is still limited to the spec's module. This is deliberate: it is the "hook only the spec calls" pattern.
  4. **Default bodies are checked in the spec's declaring module.** A spec's default bodies are authored in the spec's module, so they resolve names and visibility there, not in the `meet`'s module. This is needed for (3): a default body calling a hidden sibling requirement must work when the `meet` lives elsewhere.
  5. **Enforce requirement visibility at every call form.** That means method-call syntax, generic-bound calls, `Spec::req(x)` / fully-qualified spec calls, and dynamic dispatch on `*spec S`. `docs/language/visibility.md` already requires that dynamic dispatch not widen visibility.
- **Current behavior this fixes** (probed with `omgc-debug` against the current tree):
  - `t.hook()` in `f<T: Api>` is rejected **even in `Api`'s own module** (owner check against `current_owner`).
  - `Api::hook(d)` and `(<*spec Api>d).hook()` are accepted **from any module** because neither path checks requirement visibility.
  - A `meet` body in `Dog`'s module can't read `Dog`'s hidden field.
  - A struct literal in `Dog`'s own module (outside `Dog`'s methods) can't set a hidden field.
  - A default body whose `meet` is in another module fails to resolve a hidden free function of the spec's module ("cannot find 'helper'"), because `compile/bodies.rs` analyzes pending default bodies in the conformance's module.
  - Spec-sourced methods currently compute `shared` against the *receiver's* package. For an external spec conformed by a local type, a `shared` requirement therefore becomes callable in the wrong package.
  - Primitive inherent methods get an empty declaring module (`declaring_owner()` is `None` for primitives, and `require_method_visible` falls back to `Vec::new()`). A non-`exposed` primitive method is therefore unreachable even from `core`.
- **Rejected alternatives:**
  - Keeping owner-scoped `hidden` and adding a new keyword for module scope: two mechanisms for one concept.
  - Using the receiver/conforming type's module as a requirement's declaring module: this ties spec-member visibility to whoever implements the spec, which is the current bug.
  - Deriving a method's module at each check site (receiver owner vs. `source` spec vs. primitive lookup): one field set at construction has one source of truth and covers primitives, which have no owner.
  - Turning `source` into an origin enum (`Inherent | Conformance | Primitive { module }`) to avoid repeating the owner's `module_path` on inherent methods: the governing module is settled where the method is built, and only that code knows it for conformance methods. The repetition is harmless because the data never changes. The enum would keep a per-kind branch at every check site, while the field turns every member check into the same `(visibility, module)` pair as item checks. Keep `source` as it is; it still serves dispatch and conformance bookkeeping.
  - Forbidding a cross-module `meet` for a spec with hidden no-default requirements: this needlessly bans a legitimate pattern.

## Technical Details
- **Initial context boundary:**
  - `compiler/omega-analyzer/src/analysis/{visibility.rs, calls/, paths.rs, literals.rs, places/fields.rs, items/bodies.rs}`;
  - `compiler/omega-analyzer/src/resolved_type.rs` (`ResolvedMethod`);
  - `compiler/omega-driver/src/compile/bodies.rs` (only the pending-default-body loop);
  - the `ResolvedMethod` construction sites in `omega-driver`: `items/methods.rs`, `resolver.rs` (~1080), `primitives.rs` (~176);
  - `docs/language/visibility.md` and the specs section of `docs/language/specs-and-conformance.md`.
- **Affected files/symbols:**
  - `analysis/visibility.rs`:
    - `check_member_visibility`: drop the `Visibility::Hidden => current_owner…` branch. Hidden members go through `visibility_allows` with `origin_module(origin)`, exactly like items, so macro-authored names get the macro definition module's rights.
    - Simplify the signature: `owner_id` is no longer needed. Update all callers (`literals.rs` ×2, `paths.rs:~1086`, `calls/generic.rs` ×2, `calls/overload.rs:~109`, `calls/mod.rs::require_method_visible`, `places/fields.rs::require_visible_member`).
  - **`ResolvedMethod.declaring_module: Vec<Ident>`** (`resolved_type.rs:~205`). Set it at every construction site:
    - `analysis/items/mod.rs:~854` → the analyzer's `module_path`;
    - `analysis/specs.rs:~198` and `~210` → the spec cell's `module_path`;
    - `driver/items/methods.rs:~170` and `driver/resolver.rs:~1080` → the item key's module;
    - `driver/primitives.rs:~176` → the primitive template's module;
    - `resolved_type/tests.rs` fixtures.
    
    Every method-visibility site passes `method.declaring_module`. Remove the `declaring_owner().unwrap_or_else(|| (Vec::new(), …))` fallbacks that existed only to feed visibility; keep `declaring_owner` where it serves other purposes. Fields keep the aggregate's `module_path`.
  - `analysis/mod.rs`: delete the `current_owner` field and `with_owner`.
  - `analysis/items/bodies.rs`:
    - drop the `with_owner` wrappers in `check_struct_body`, `check_union_body`, `check_enum_body` and `check_pending_spec_method`;
    - drop the comment at ~159 that mirrors them.
  - `analysis/calls/mod.rs::finish_dynamic_dispatch_call`: after selecting the slot, check the requirement's visibility against the declaring spec's module. The flattened function carries its `spec_name`; confirm it can reach the spec cell and module, or extend the flattened record minimally. Emit `MethodNotVisible` and honor `reveal` through the existing `revealed` / `with_reveal_operand` path.
  - `analysis/calls/spec.rs` (`resolve_fully_qualified_spec_call`, `resolve_static_spec_call`, `resolve_instance_spec_call`): check the selected requirement's visibility against the spec's module. The spec path itself is already checked in `specs.rs:~486`; the member currently is not.
  - `omega-driver/src/compile/bodies.rs`, `for pending in &entry.pending`: run `with_analyzer_in` in the spec's declaring module (`entry.spec.borrow().module_path`) instead of `path`. Keep `conformance_owner`, symbol ownership and emission bookkeeping unchanged.
- **Interfaces/invariants:**
  - Top-level item visibility, `shared`, `exposed`, alias gating, import `reveal`, and binary/`@symbol(export)` visibility are unchanged.
  - A spec member is never more visible than its spec (the existing cap).
  - Dynamic dispatch never widens visibility.
  - `reveal` still bypasses any failing member check and still warns when unnecessary. After step 3, `reveal Greeter::double_name(&dog)` in `t13_visibility` becomes genuinely necessary.
  - Moving default-body analysis must not change generated symbols or which package emits the instantiation. `conformance_owner` is still the entry's owner.
- **Out of scope:**
  - `reveal` place-resolution debt (`design-debt.md`).
  - Any change to the `@suppress`/redundant-`hidden` warning logic.
- **Risks/open questions (escalate, don't improvise):**
  - If analyzing a default body in the spec's module breaks `Self`-substitution or bound context for specs from **external** packages (e.g. `core` specs with defaults), stop and report. Don't fall back to the conformance module for some cases.
  - If `finish_dynamic_dispatch_call` has no cheap path from a flattened slot to its declaring spec's module, report the needed representation change before widening the flatten record.

## Implementation Plan
1. **Recorded declaring module and single-rule check.**
   - Add `ResolvedMethod.declaring_module` and populate it at every construction site.
   - Rewrite `check_member_visibility` to delegate to `visibility_allows` (drop `owner_id`) and update every caller to pass `method.declaring_module` (or the aggregate module for fields).
   - Remove `current_owner` / `with_owner` and their uses in `items/bodies.rs`.
   - Build and run `cargo test -p omega-analyzer -p omega-driver`.
2. **Default bodies in the spec's module.** Change the pending loop in `omega-driver/src/compile/bodies.rs` to analyze in `entry.spec`'s module.
3. **Close the enforcement gaps.** Add requirement-visibility checks to the spec-qualified call paths in `calls/spec.rs` and to `finish_dynamic_dispatch_call`.
4. **Update docs** (normative first):
   - `docs/language/visibility.md`:
     - Rewrite "Hidden items and hidden members": hidden means visible throughout the declaring module for items and members alike. Full field sealing = the type alone in its module.
     - Macros section: remove the owner-only paragraph. Macro-authored member names are checked with the definition module's rights like any other reference.
     - Specs section: state that a requirement's declaring module is the spec's module, and that a `meet` elsewhere may implement a hidden requirement it cannot call. Rewrite the `double_name` example comment ("reachable only from other methods of this same spec" → "reachable throughout the spec's module").
   - `docs/language/specs-and-conformance.md`: state that default bodies are resolved and checked in the spec's declaring module.
   - `docs/guide/quick-reference.md:~606`: keep it consistent. The spec-member example there remains valid.
   - `docs/architecture/semantic-analysis.md:~20`: remove "current aggregate owner for hidden member access".
5. **Update tests** (see Testing). Also rename `macro_hygiene.rs::a_macro_authored_member_name_does_not_inherit_the_invocation_site_owner` to say "module" rather than "owner". Its assertion still holds because `helper` ≠ the `Box` module.

## Testing
- **Changed conformance case `tests/t13_visibility`** (positive, `expected.stdout`):
  - Update `child/child.omg` comments that claim owner-only member visibility.
  - In `child`, add:
    - a free function reading `Box.hidden_field` and calling `hidden_double` without `reveal`;
    - a struct literal setting a hidden field outside the type's methods;
    - a `meet` body reading its target's hidden field;
    - a generic `f<T: Greeter>` in `child` calling `double_name`, instantiated from `child`.
  - Add a cross-module conformance: a spec in `child` with a hidden requirement whose default body calls a hidden `child` free function, conformed by a type declared in the root module, with a spec-module entry point exercising it. This proves step 2.
- **New negative case `tests/t13b_visibility_errors`** (`expected.stderr`, exact diagnostics). From a module other than the declaring one, each of the following must produce `FieldNotVisible` / `MethodNotVisible` (or the item not-visible error):
  - a hidden field read;
  - a hidden inherent-method call;
  - a hidden requirement called via `x.req()` through a generic bound;
  - a hidden requirement called via `Spec::req(x)`;
  - a hidden requirement called via `<*spec S>` dynamic dispatch;
  - a `meet` in another module whose own body calls the hidden requirement it implements;
  - a `shared` requirement of an `exposed` spec reached from another package, if a dependency package is practical in the root suite. Otherwise cover it with a `omega-driver` component test.
  
  Include one `reveal` on a hidden member that now warns as unnecessary because it is same-module.
- **Specification trace:** `docs/language/visibility.md` (hidden items and members, macros, specs and conformance, dynamic dispatch must not widen visibility); `docs/language/specs-and-conformance.md` (default-body resolution site).
- **Component tests:** `compiler/omega-driver/tests/macro_hygiene.rs`. Add a macro defined in the owner's own module that names a hidden field without `reveal` and compiles.
- **Primitive methods:** a user package cannot declare a `primitive` block, so cover this with an `omega-driver` component test if its harness can supply a `core` stand-in. Otherwise rely on the runtime build plus an assertion that a `shared` primitive method in `core` is callable from another `core` module. If neither is practical, report it rather than skipping silently.
- **Regression coverage:** `t18b_macro_visibility_errors`, `t26_aliases`, `t26b_alias_errors`, `t40_symbol_visibility`, `t40b_symbol_visibility_access`, `t05k_generic_bound_selector_errors`, plus all spec/conformance cases. The runtime (`core`/`std`) must still build, since it contains many default bodies from external-package specs.
- **Commands:** `cargo test -p omega-analyzer -p omega-driver`; `./bin/test-runner t13_visibility t13b_visibility_errors t18b_macro_visibility_errors`; then `just test-all`.
