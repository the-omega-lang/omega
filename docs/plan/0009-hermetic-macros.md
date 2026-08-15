# Hermetic macros: definition-site name resolution

## Task Description

- **What is being asked:** make a macro body's names resolve in the module that
  *defined* the macro, not the module that invoked it. Tokens that arrive by
  metavariable substitution keep resolving in the caller's scope. A macro's
  parameter list becomes its entire interface in both directions.

- **Purpose:** three tracked defects in `docs/14-known-issues.md` have one
  cause — expansion splices tokens into the caller and resolves them there.

  1. *Every item a macro body names must also be imported by the caller.*
     `import extern::std::io::println;` does not make `println$` usable; the
     caller must also import `BufWriter`, `Stdout`/`Stderr`, `Write`, and
     `Display`. Five imports for one macro, and the failure cascade ends with
     `cannot find 'omega_print_out' in this scope`, naming an
     expansion-internal local the author never wrote.
  2. *A macro body's nested invocations resolve at the call site*, so an
     exported macro cannot call a helper macro the caller cannot also see.
  3. *Local capture.* A body's `mut out := …` binds a caller's `out` passed as
     an argument. `runtime/std/io.omg` prefixes every expansion-internal local
     with `omega_print_` for exactly this reason, after a plainer `out`
     shadowed a caller's variable in `examples/dev/dev.omg`.

  This serves "no hidden behavior" directly: today a macro's meaning depends
  on what the caller happens to have imported, which is invisible at both
  sites.

- **Reasoning:** every name resolves in the scope where it was *written*. Body
  tokens were written by the macro author, so they get the author's scope and
  the author's visibility; argument tokens were written by the caller, so they
  get the caller's. One rule, two authors.

  The mechanism is a **per-invocation expansion id** carried on `Path`, plus a
  table mapping id → defining module. Substituted tokens **keep** the id they
  arrived with, so ids never compose: one id per token, never a set. That is
  what makes this tractable where Rust's is not — no scope-set algebra, no
  transparency lattice, no mark chains.

  It is also cheap for a reason specific to this codebase: a bare variable
  reference is already `Expression::Path(ExprPath)` — the AST calls it "the
  degenerate one-segment case" — so **one** field on `Path` covers item paths,
  type references, *and* local variable references.

  Alternatives rejected:
  - *A `$crate`-style definition-site path root.* This is Rust's actual answer
    (`macro_rules!` is not hygienic for items; `println!` works by expanding to
    `$crate::io::_print`). Rejected because `$crate` exists only so the
    *author* can hand-write an absolute path — if the compiler resolves in the
    definition scope, the path is computed and there is nothing to spell.
  - *Call-site fallback ("try def-site, then caller").* Rejected: it
    reintroduces exactly the incoherence being removed, and makes a macro's
    behaviour depend on the caller's imports again, just less predictably.
  - *Rust-style syntax-context hygiene with mark composition.* Unnecessary once
    ids do not compose. Rust needs it because its contexts must model both
    def-site and call-site modes for nested expansions; full def-site hygiene
    there is `decl_macro`, unstable since 2017, still blocked partly on
    "hygiene bending". Omega's parameter rule *is* the bending mechanism.
  - Rust cannot retrofit any of this: a decade of `macro_rules!` deliberately
    resolves items at the call site. Omega has ten macros in tree, all of which
    already satisfy the strict rule.

- **Resolved concerns:**

  1. **`Self` and generic parameters must stay outside the origin model.**
     Found while studying `omega-analyzer/src/context.rs`: type names are
     resolved through `ScopeContext::defined_types` (an `IndexMap<Ident,
     ResolvedType>`), seeded by `Analyzer::new_in`/`with_substitution`
     (`analysis/mod.rs:308`, `:431`) from the *driver's* substitution list —
     `("Self", target)` plus the item's generics. There is no binding token at
     all. `runtime/std/primitives.omg` relies on this heavily:
     `conform $T to Default { default() => Self { 0 } }` and eleven more uses
     of `Self` inside macro bodies. If `defined_types` lookup became
     origin-filtered, the body's `Self` would not match the driver-injected
     binding and every one of those macros would break.

     **Decision: `defined_types` lookup ignores origin entirely.** Type
     parameters and `Self` are substitution-bound, not lexically bound, so they
     are not part of "resolve where written". The narrow cost is recorded under
     *Risks*.

  2. **Open question A — repetitions.** `$...(sep){ … }` re-emits its body per
     iteration. Giving each *iteration* a fresh id was the obvious answer and
     is **wrong**: the existing print macros reference an outer local from
     inside a repetition —
     `$...(){ Display::fmt($args, &mut omega_print_out); }` — where
     `omega_print_out` is declared outside the repetition. A per-iteration id
     would make that reference fail to match its declaration.

     **Decision: one id per invocation, not per emission.** A repetition body
     that *declares* a local therefore emits N same-origin declarations into
     one scope and gets the **existing** `Redeclaration` diagnostic, exactly as
     hand-written code would. No new rule and no new check: the pre-existing
     scoping rule applies uniformly, and the author's fix (wrap the repetition
     body in `{ }`) is what they wanted anyway.

  3. **Open question B — macro-generated imports.** Under the hermetic rule an
     `import` in a body is incoherent: the body's own names already resolve in
     the defining module, so the import cannot affect them, and its only
     remaining effect is to silently mutate the *caller's* namespace with a
     name the caller never wrote. `docs/12-macros.md` already records that such
     imports "arrive too late."

     **Decision: reject an `import` inside a macro body at definition time**,
     with a dedicated parse diagnostic. This converts a silent misbehaviour
     into an error and removes a feature that the new rule makes meaningless.

  4. **`Path` derives `Hash`/`Eq`.** Verified that nothing keys a map or set on
     `Path`/`ExprPath` and nothing compares raw `Path`/`Type` for equality
     outside `ResolvedType`. Adding a field is therefore safe — but the new
     field must still be **excluded from `PartialEq`/`Hash` by hand-written
     impls**, so that no existing structural comparison can change meaning as a
     side effect. `Ident` (`pub struct Ident(pub String)`) must not be touched
     at all: it is the key of `declared_variables`, `defined_types`, every
     generic substitution map, and several `HashSet`s.

## Technical Details

### What changes

| File | Change |
|---|---|
| `omega-parser/src/ast/identifier.rs` | `Path` gains `origin: Origin`; manual `PartialEq`/`Hash` excluding it. |
| `omega-parser/src/macros.rs` | Assigns one `ExpansionId` per invocation; tags body-emitted paths; leaves substituted paths untouched; rejects `import` in a body; resolves nested invocations in the defining module's macro environment. |
| `omega-parser/src/ast/statement/{walrus,declaration}.rs` | Binding statements carry `Origin` on their name slot. |
| `omega-parser/src/diagnostics.rs` | New `ParseErrorKind::ImportInMacroBody`. |
| `omega-hir/src/hir.rs`, `lower.rs` | Nothing structural — HIR already `use`s `Path`/`ExprPath` from `omega_parser::prelude` and reuses them, so `origin` propagates for free. Verify `lower.rs` does not reconstruct paths field-by-field anywhere. |
| `omega-analyzer/src/context.rs` | `declared_variables` key becomes `(Ident, Origin)`; `find_variable` matches on the pair; `best_match` projects back to `Ident`. `defined_types` **unchanged**. |
| `omega-analyzer/src/analysis/paths.rs`, `omega-driver/src/resolver.rs` | Module-scope item resolution consults `path.origin` → defining module. |
| `omega-analyzer/src/analysis/*` | Unused-variable warnings suppressed for non-caller-origin locals. |
| `runtime/std/io.omg` | Drop `omega_print_` prefixes (they exist only to work around capture). |
| `examples/**` | Delete now-redundant imports — this is the end-to-end proof. |
| `docs/{07,12,14,24}` | See step 10. |

### What must not change

- **`Ident`.** No field, no changed equality. See *Resolved concerns 4*.
- **`defined_types` / `Self` / generic parameters.** See *Resolved concerns 1*.
- **Span re-anchoring.** Generated spans keep pointing at the invocation.
  `Span` carries no file identity, so definition-site spans would index into
  the wrong file's text. This is why the macro-author lint cannot be recovered.
- **Method and field resolution.** Type-directed, not scope-directed, so
  `self.describe()` and `self.buf` inside a body are unaffected. This is what
  makes the strict rule livable — it constrains only *path* references.
- **Codegen and ABI.** `linkage_for` (`omega-codegen/src/cranelift/item.rs:36`)
  keys only on `type_args`; visibility never reaches codegen and everything is
  `Linkage::Export`. The new visibility rule is pure front-end policy. Runtime
  object symbol tables must come out byte-identical.
- **The "duck-typed expansion" property** (`docs/12-macros.md`). A body still
  cannot be parsed or checked standalone — a `$T` may sit in type position and
  repetitions change arity. Only the *scope* a reference is looked up in
  changes, never *when* it is looked up.

### Chosen approach

```rust
/// Which expansion emitted a token, or `None` for text the author of the
/// module being compiled wrote directly. Substituted tokens keep the origin
/// they arrived with, so this is a single id and never a set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Origin(pub Option<ExpansionId>);
```

`Path { head, tail, origin }`. Resolution asks one question — "which module was
this token written in?" — answered by `Origin` → the expansion table → the
defining `ModulePath`, or the module under compilation when `None`.

Three consumers, deliberately named because they sit in two different phases:

1. **Macro-name lookup**, in the parser/expander. Fixes defect 2.
2. **Item/type path resolution**, in the analyzer and driver. Fixes defect 1.
3. **Local variable lookup**, in `Context`. Fixes defect 3.

`MacroDefinitionStmt` currently carries no module identity —
`macros::expand(module, imported: &HashMap<Ident, MacroDefinitionStmt>)`
receives a flat map merged from the caller's own definitions and its imports
(`macros.rs:214`). Giving each definition its defining `ModulePath` is the
prerequisite for all three consumers and is step 1.

**Visibility:** an item a macro body names must be at least as visible as the
macro itself. `exposed` macro → `exposed` items only; `internal` macro →
`internal` items, which covers every intra-package case unchanged. Precedent:
composed methods inherit their spec's visibility and may not exceed it. This
avoids Rust's `#[doc(hidden)] pub` wart while keeping the promise that
visibility levels mean something — they are unenforced at link time, so an
exposed macro quietly pulling internals into a foreign translation unit would
make that promise false where nobody can see it.

**Declarations are not resolution.** Origin is never consulted on a
declaration's name slot. The consequence is that a name a body introduces is
inaccessible to the caller — uniformly for locals *and* items — unless it
arrived as a parameter. Verified this costs nothing: every macro in the tree
emits only anonymous declarations (`primitive $T { }` in
`runtime/core/numerics.omg`, `conform $T to Spec { }` in
`runtime/std/primitives.omg`, no items at all in `runtime/std/io.omg`), and
method names inside those blocks are reached through the type or spec
namespace, which is type-directed.

### Risks and open questions

- **Type-level capture survives.** Because `defined_types` ignores origin, a
  caller passing a type name that collides with a generic parameter of a
  declaration the macro generates will bind to the generated parameter. Narrow,
  no in-tree instance, and the alternative breaks every `Self` in
  `runtime/std/primitives.omg`. Record in `docs/14`.
- **The macro author loses the unused-local lint** and cannot get it back:
  reporting it against the macro needs a span pointing into the macro's source,
  which `Span` cannot express. Same root cause as the composite-span entry.
- **Diagnostic quality where two same-spelled locals coexist.** Both anchor at
  the invocation span. The executing agent should check what
  `Redeclaration` and "did you mean" print in that case and **flag it rather
  than invent a span scheme.**
- **`lower.rs` path reconstruction.** If HIR lowering builds any `Path` by
  struct literal rather than moving the parser's, origin will be silently
  dropped there. Grep before assuming the "free" propagation holds.

## Implementation Plan

Steps 1–2 are prerequisites with no behaviour change; the tree must stay green
through both.

1. **Give macro definitions a home module.** Introduce `ExpansionId` and
   `Origin` in `omega-parser`. Change `macros::expand`'s `imported` map
   (`macros.rs:214`) to carry each definition's defining `ModulePath`, and
   update the driver site that builds it. Add the expansion table (id →
   defining module) as an output of `expand`, threaded to the analyzer. No
   resolution changes yet.

2. **Add `origin` to `Path`** (`ast/identifier.rs:25`) with hand-written
   `PartialEq`/`Hash` that exclude it, and to the name slot of `WalrusStmt` and
   `DeclarationStmt`. Default `Origin(None)` everywhere. Grep `omega-hir/src/
   lower.rs` for any `Path { … }` struct literal and thread origin through it.

3. **Tag body-emitted paths in the expander.** In `substitute_invocation`
   (`macros.rs:545`) allocate one fresh `ExpansionId` per invocation; stamp it
   on paths built from body tokens; leave paths from substituted arguments
   untouched. Repetitions share the invocation's id — see *Resolved concerns 2*.

4. **Nested macro invocations resolve at the definition site.** Macro-name
   lookup inside a body consults the emitting expansion's defining module's
   macro environment rather than the caller's merged map. Fixes defect 2.

5. **Item and type path resolution consults origin.** In
   `omega-analyzer/src/analysis/paths.rs` and `omega-driver/src/resolver.rs`,
   resolve an unqualified or module-relative path against the origin's defining
   module instead of the module under compilation. Strictly — no fallback.
   Fixes defect 1.

6. **Local variable resolution partitions by origin.** In
   `omega-analyzer/src/context.rs`, change `declared_variables` from
   `IndexMap<Ident, VarBinding>` to `IndexMap<(Ident, Origin), VarBinding>`;
   update `declare`, `find_variable`, the innermost-first walk, and the
   `best_match` call over `declared_variables.keys()` (`context.rs:237`) to
   project keys back to `Ident`. Fixes defect 3. **Do not touch
   `defined_types`, whose own `best_match` at `context.rs:244` stays as it is.**

7. **Suppress unused-variable warnings for non-caller-origin locals**, and add
   a test that passing a caller variable into a macro still marks it *used*.

8. **Visibility rule.** An item named by a macro body must be at least as
   visible as the macro. New `AnalysisErrorKind`; check where step 5 resolves.

9. **Reject `import` in a macro body** at definition time, in
   `macros::validate_definition` (`macros.rs:269`, already called for every
   definition from `expand` at `macros.rs:222`), new
   `ParseErrorKind::ImportInMacroBody`.

10. **Cleanups and docs.** Drop the `omega_print_` prefixes in
    `runtime/std/io.omg`; delete now-redundant imports across `examples/`.
    Rewrite `docs/12-macros.md`'s "Why no gensym/hygiene machinery exists"
    section (now largely wrong); update `docs/24-console-io.md` ("Omega macros
    are textual and unhygienic"); add the macro/item visibility rule to
    `docs/07-visibility.md`; in `docs/14-known-issues.md` remove the three
    fixed entries and add the two new limitations from *Risks*. Archive this
    file as `docs/plan/0009-hermetic-macros.md`.

## Testing

**New cases** (`compiler/omega-driver/tests/`, using the `TestPackage`
harness):

- *Step 5, the discriminating test:* a caller that **shadows** a name the macro
  body uses — caller declares its own `Display` (or a same-named function
  returning a different value) and invokes a macro from a child module whose
  body names `Display`. Assert the expansion resolved to the **definition's**
  one. Assert on the resolved `decl_id`/emitted body, not on "it compiled" —
  the analogous observability gap from the blanket-conformance work is still
  open in `docs/14:122` and must not be repeated here.
- *Step 5:* a macro whose body names an item the caller does **not** import
  compiles. This is defect 1; today it fails.
- *Step 6:* `print$(out)` where the caller has its own local `out` — currently
  the motivating capture bug. Assert the argument bound to the caller's.
- *Step 4:* an exported macro calling a helper macro the caller cannot see.
- *Step 3 + Resolved concerns 2:* a repetition body referencing a local
  declared outside the repetition still compiles (this is the shape
  `println$` uses, so `just test-io` also covers it).
- *Step 7:* passing a variable to a macro marks it used — no false
  `unused variable`. Two false-positive classes have already shipped here.

**Negative cases** — message quality is part of the deliverable:

- A body naming an item that does not exist in the *defining* module → the
  error must name the macro's module, not the caller's, and must not mention
  expansion-internal locals the author never wrote.
- `exposed` macro naming an `internal` item → new visibility diagnostic, with
  help text pointing at the item's declaration.
- `import` inside a macro body → `ImportInMacroBody` at the definition.
- A repetition body that declares a local without its own block →
  the existing `Redeclaration`. Confirm the message is comprehensible given
  both spans anchor at the invocation; **flag it if not** rather than
  inventing a span scheme.

**Regression risk**, in order:

1. `runtime/std/primitives.omg` and `runtime/core/numerics.omg` — twelve uses
   of `Self` inside macro bodies. If step 6 is over-applied to `defined_types`,
   all of them break. This is the single most likely failure.
2. `just test-io`, `test-stdio-contract`, `test-multi-print` — every print path
   goes through the four macros being changed in step 10.
3. `cargo test` — 123 tests across 21 suites.
4. **Symbol stability:** `core`/`std`/`plat` object symbol tables must be
   byte-identical before and after. Verify against a detached-worktree baseline
   with `diff <(nm --defined-only base.o | sort) <(nm --defined-only new.o |
   sort)`. Renaming the `omega_print_` locals in step 10 must not move a single
   symbol — locals are not symbols, so any diff means something else moved.

**Target coverage:** `just test-core-only` (freestanding, no allocator) must
stay clean — this change is entirely front-end and must add nothing to
`core.o`, which today has zero relocations.
