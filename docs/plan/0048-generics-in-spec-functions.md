# Generic spec requirements

## Task Description

- **What is being asked:** a `spec` requirement may declare its own generic
  parameters, and a `meet` block may implement it.

  ```omega
  spec Runner {
      execute<T>(*self, task: T) => T;
  }

  meet Runner for Direct {
      execute<T>(*self, task: T) => T { task }
  }
  ```

  Today this does not parse: `parse_spec_function`
  (`omega-parser/src/parser/item/definitions.rs`) goes straight from the
  requirement's name to `(`, so `<T>` is a syntax error before any semantic
  question arises.

- **Purpose:** a spec is Omega's only way to name a capability, and without
  generic requirements a capability that is *itself* generic cannot be named
  at all. `execute<T>(task: T) => T`, `map<U>(f: (T) => U) => Container<U>`,
  and every "do this for a caller-chosen type" interface currently have to be
  written as inherent methods per type, which defeats the point of having
  specs. Inherent and static generic declarations already work
  (`tests/t10c_generic_member_functions`); specs are the remaining hole.

- **Reasoning:** the declaration side is nearly free — `RawSpecFunctionSig`
  (`omega-analyzer/src/resolved_type.rs`) already stores requirements as
  *raw* syntax (`Vec<HirParam>` + `Type`), never as resolved function types,
  so carrying a generic parameter list through parser → HIR → spec
  declaration adds a field and no new resolution. The cost is entirely on the
  implementation and call sides, where a `meet` block's declaration becomes a
  template with no signature until a call determines its arguments — the same
  shape as the inherent-method work, but owned by a conformance entry rather
  than by an item query.

- **Resolved concerns:**
  - **Two different checks, not one.** Whether an implementation *matches its
    requirement* is answerable from the declarations alone and is checked
    strictly at the `meet` block. Whether a particular `T` *works* is
    answerable only per instantiation and is checked there. Deferring the
    first would lose an error the compiler can always produce; hoisting the
    second is impossible in a language whose unbounded generics are
    duck-typed. Rust draws the same line (`E0049`/`E0276` at the impl,
    monomorphization for the rest); the split carries more weight here
    because Omega checks generic bodies per instantiation rather than once
    against declared bounds.
  - **The strict check cannot compare fully resolved signatures.** Comparing
    two templates means resolving `T` to something, and `ResolvedType` has no
    generic-parameter variant — that is the `GenericParamId` work already
    recorded in [`docs/issues/design-debt.md`](docs/issues/design-debt.md),
    which this plan does not open. Instead the comparison delegates to the
    real type engine everywhere a written type mentions no generic parameter,
    and falls back to a positional structural comparison only in the
    positions that do (`T`, `*T`, `[N]T`, `Node<T>`). Both sides get
    `aliases::expand_type_alias` first, so alias spelling cannot make two
    equal signatures compare unequal.
  - **Object safety is whole-spec, not per-requirement.** A generic
    requirement has one code address per instantiation and therefore no
    vtable slot, and the set of instantiations is not knowable where dispatch
    metadata is emitted — a separately compiled package can name argument
    types the defining package never saw, which "Separate compilation is
    real" (`ARCHITECTURE.md`) forbids assuming away. Omega already answers
    this exact question whole-spec for `=> spec S` return requirements
    (`docs/language/specs-and-conformance.md`), and giving generic
    requirements a *different* answer would leave the language with two rules
    for one situation. Rust's per-method escape hatch (`where Self: Sized`)
    is a deliberate opt-in marker; adding an equivalent to Omega is a
    language feature that should be designed once for both rules, not
    invented inside this change.
  - **Default bodies are supported, not rejected.** A default body on a
    generic requirement is a template like any other, and the instantiation
    path built here serves it with the body coming from the spec instead of
    the `meet` block. Rust allows it, and rejecting it would be an arbitrary
    hole.

## Technical Details

### What changes

- `SpecFunctionStmt` gains `generics`, parsed by `parse_spec_function`.
- `HirSpecFunction` and `RawSpecFunctionSig` carry that list.
- `FlattenedSpecFn`'s signature becomes a two-variant enum so every consumer
  decides what a template means instead of silently reading a `fn_type`.
- `check_conform_block` matches generic requirements against generic
  implementations by declared shape, and keeps both out of the conformance's
  concrete method list.
- Conformance-method templates get an instantiation query, sharing the
  machinery built for inherent methods.
- A spec with a generic requirement is not object-safe.
- Call sites that reach a conforming method — instance calls, bound-provided
  calls, type-qualified paths, qualified spec calls — consult templates.

### What must not change

- A non-generic requirement's resolution, matching, dispatch, vtable layout,
  and symbol must be byte-for-byte what they are today. `conformance_method_symbol`
  gains a method-generic-arguments parameter that is empty for every existing
  caller, exactly as `method_symbol` did.
- `gap` blocks share `SpecFunctionStmt`; a generic gap function stays
  rejected (a gap function is a linkage-level binding with one address).
- Object-safe specs keep forming `*spec S` unchanged.
- The `=> spec S` object-safety rule and its diagnostic are untouched.

### Chosen approach

**One instantiation path, two kinds of owner.** The inherent-method work
introduced `MethodKey { owner: ItemKey, method: HirId, generic_args }`
(`omega-driver/src/items/methods.rs`). Rather than add a second, parallel
conformance-method key — the exact pattern
[`docs/issues/design-debt.md`](docs/issues/design-debt.md) records as debt —
generalize the owner:

```rust
enum InstantiationOwner {
    Item(ItemKey),                 // inherent declaration on a struct/union/enum
    Conformance(ConformanceKey),   // declaration inside a `meet` block
}
```

`ConformanceKey` is the identity `Conformances::emitted` already uses:
`(target lookup key, spec id, spec_args)`. A `ConformanceEntry`
(`omega-driver/src/conformances/mod.rs`) already carries everything an
instantiation needs — `functions: Vec<HirFunctionDef>`, `substitution`,
`declared_bounds`, `module` — so the existing `instantiate_generic_method`
body works for both owners once it resolves the template and enclosing
substitution through the owner rather than through `owner_item_key`.

Emission reuses what exists: a conformance-method instantiation is a
standalone `CheckedFunctionDef` with `conformance_owner` already set (the
field predates this work), plus `generic_args`; `function_linkage`
(`omega-mir/src/lower/item.rs`) already returns `MirLinkage::Weak` for a
non-empty `generic_args`, so duplicate monomorphizations across packages fold
without further change.

**Shape comparison** lives in one new module,
`omega-analyzer/src/analysis/spec_shape.rs`, with a single entry point:

```rust
fn requirement_shape_matches(
    &mut self,
    requirement: &RawSpecFunctionSig,   // + the spec's substitution/module
    implementation: &HirFunctionDef,    // + the meet block's module
) -> Result<(), ShapeMismatch>
```

Rules, in order, each producing its own diagnostic:

1. generic parameter count;
2. per position, parameter kind (type vs `comp`, and a `comp`'s declared
   value type resolved on both sides);
3. per position, declared bounds, compared as resolved spec applications;
4. receiver form, parameter count, variadicity;
5. per parameter and the return type: expand aliases on both sides
   (`aliases::expand_type_alias`), then compare — a written type mentioning
   none of the declaration's own generic parameters is resolved on both sides
   and compared as `ResolvedType`; one that does is compared structurally
   with each side's parameters as positional holes.

Parameter *names* are not compared: `execute<T>` may be implemented by
`execute<U>`.

**`FlattenedSpecFn`** becomes:

```rust
enum RequirementSignature {
    Concrete { fn_type: ResolvedFunctionType, return_type_bound: Vec<...> },
    Template { generics: Vec<HirGenericParam> },
}
```

Making this an enum rather than an `Option<ResolvedFunctionType>` is
deliberate: the vtable/shape builder, the missing-method diagnostic,
`type_implements_spec`, and `fn_satisfies_requirement` must each state what a
template means, and the compiler should refuse to build if a new consumer
forgets.

### Risks and open questions

- **The shape comparator is a second engine.** It is bounded by delegating to
  real resolution outside parameter-mentioning positions and by sharing the
  one canonical alias expansion, but a divergence between it and
  `Context::resolve_type` would show up as a wrongly accepted or wrongly
  rejected `meet` block. Mitigation: the fallback path is deliberately tiny,
  and step 4's tests pin the delegating cases.
- **Two substitution layers.** A generic `meet` block (blanket conformance)
  has its own generics *and* the method's. The shadowing rule established for
  inherent methods applies unchanged — the method's parameters are pushed
  first so an inner name wins — but the interaction needs a dedicated test.
- **`comp` parameters inside requirement types** (`[N]T` as a parameter type)
  put a generic parameter in an array length, which the structural comparison
  must treat as a hole rather than as a value.
- **A default body that is never called is never checked**, exactly like any
  other uninstantiated template. Consistent, but worth stating in the docs.
- **`primitive` blocks are out of scope.** They reject generic declarations
  today and continue to; the owner enum above is the extension point if that
  changes, and the existing limitation entry stays.

## Implementation Plan

Each step builds and passes `cargo test` + `just test-all` before the next.

### Step 1 — grammar

`omega-parser`: `parse_spec_function` calls `parse_optional_generics` after
the name; `SpecFunctionStmt.generics`. `parse_gap_def` rejects a non-empty
list on a gap function with a dedicated error (reuse the existing
gap/glue-generics rejection style). Update `docs/language/grammar.md`'s
`spec-function` rule.

*Verify:* new `omega-parser/tests` cases — a generic requirement parses; a
generic gap function is rejected.

### Step 2 — declaration

`omega-hir`: `HirSpecFunction.generics`, lowered from the AST.
`omega-analyzer`: `RawSpecFunctionSig.generics`, populated in
`resolve_spec_functions` (`analysis/specs.rs`).
`omega-driver`: `resolve_spec_declaration` (`items/resolution.rs`) extends
`is_object_safe` with "no requirement declares generics".

*Verify:* a spec with a generic requirement compiles when nothing implements
it; `*spec Runner` is rejected with the existing `SpecNotObjectSafe`.

### Step 3 — requirement signatures

Introduce `RequirementSignature` and thread it through `flatten_spec` /
`flatten_spec_into`. Every consumer decides explicitly:
`fn_satisfies_requirement`, `type_implements_spec`, the missing-requirement
diagnostic, `check_conform_block`, and the dynamic-dispatch shape builder
(which can now assert templates never reach it, since step 2 made such specs
non-object-safe).

*Verify:* full suite green with no behavior change for non-generic specs.

### Step 4 — shape matching at the `meet` block

New `analysis/spec_shape.rs` implementing the rules above, plus its
diagnostics. `check_conform_block` uses it: a generic requirement is
satisfied only by a generic implementation whose shape matches; the pair is
recorded as a template on the `ConformanceEntry` instead of resolved into
`methods`. A generic implementation with no matching requirement, or a
non-generic implementation of a generic requirement (and the reverse), is an
error at the `meet` block.

*Verify:* negative driver tests for each mismatch class; a matching pair
compiles with no call site present.

### Step 5 — instantiation

Generalize `MethodKey.owner` to `InstantiationOwner`; teach
`items/methods.rs` to resolve a template and its enclosing substitution
through either owner. Extend the driver's `generic_method_template` /
`instantiate_generic_method` to consult conformance templates for the target
type after inherent lookup fails. `conformance_method_symbol` gains the
method's generic arguments.

*Verify:* driver test asserting the demangled symbol of a conformance-method
instantiation and its weak linkage, mirroring
`compiler/omega-driver/tests/generic_methods.rs`.

### Step 6 — call sites

Wire template lookup into the four paths that can reach a conforming method:

- `find_functions` (`analysis/places/mod.rs`) — the conformance branch and
  the `self.bounds` branch;
- `resolve_type_member` (`analysis/paths.rs`) — conformance providers;
- `resolve_generic_method_call` (`analysis/calls/generic.rs`) — the
  type-qualified interceptor, via the extended driver query;
- `calls/spec.rs` — qualified `<T: Spec>::method(...)` calls.

*Verify:* the conformance case from step 8 exercises all four spellings.

### Step 7 — default bodies

A generic requirement with a default body becomes a template instantiated per
(conforming type × method arguments) through the step 5 path, with the body
taken from `RawSpecFunctionSig::default_body`. `PendingSpecMethod` keeps
serving non-generic defaults unchanged.

*Verify:* a conforming type that omits a defaulted generic requirement still
compiles and calls the default.

### Step 8 — tests and docs

Conformance cases `tests/t11b_generic_spec_requirements` (positive: all four
call spellings, a bound-qualified call, a generic spec *and* generic
requirement together, a blanket conformance, a default body) and
`tests/t11c_generic_spec_requirement_errors` (shape mismatches, `*spec S`
rejection, generic gap function).

Docs: `specs-and-conformance.md` gains a generic-requirements section stating
the two checks, the whole-spec object-safety consequence, and the
generic-spec alternative (`spec Runner<T>`) that the diagnostic should point
at; `generics.md` and `functions.md` cross-reference it; `grammar.md` from
step 1; `docs/issues/language-limitations.md` records what the shape check
cannot see and that per-requirement dyn exclusion does not exist;
`docs/architecture/semantic-analysis.md` and `module-driver-and-linkage.md`
describe the shared instantiation owner, and the design-debt entry on method
instantiation is updated to say conformance methods now share that path.

## Testing

- **Parser** (`omega-parser/tests/`): generic requirement parses with bounds,
  defaults, and `comp` parameters; generic gap function rejected.
- **Driver** (`omega-driver/tests/generic_spec_requirements.rs`): shape
  match/mismatch classes each with their own diagnostic; instantiation symbol
  and weak linkage; one instantiation shared by two call sites; a template
  nobody calls emits nothing; `*spec Runner` rejected.
- **Conformance** (`tests/t11b*`, `tests/t11c*`): observable behavior end to
  end through compile/link/run, including a separately compiled package
  instantiating a conformance method the defining package never did.
- **Regression:** `just test-all` must stay green throughout; step 3 is the
  one with real blast radius, so run the full suite before proceeding past it.
