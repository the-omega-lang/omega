# Known issues tracker

A single consolidated list of every confirmed, currently-unfixed gap
described in these docs, for tracking at a glance. Each entry links to its
full writeup. Update this file whenever a gap here is fixed (move it to a
"Fixed" note in the relevant topic file, don't just delete the line) or a
new one is found.

## Codegen

- **A range-driven `for` loop no longer compiles to a bare three-clause
  loop, and will not until MIR-level optimization exists.** `for i in 0..<n`
  used to be intercepted by a dedicated analyzer desugaring that emitted a
  counter, a comparison and an increment directly. It is now an ordinary
  `ToIterator`/`Iterator` call chain over `core::range::Range<T>` and
  `RangeIterator<T>`, which is what makes ranges tangible values and removes
  every range special case from the compiler — but it means the emitted code
  is a `next()` call returning `Option<T>`, plus a match, per iteration.
  Recovering the old shape needs two MIR passes that do not exist yet:
  inlining, and scalar replacement of aggregates to dissolve the cursor
  struct into registers. **Cranelift will not do this for us** — its
  optimizer is far weaker than LLVM's here, and LLVM is what makes the
  equivalent Rust code collapse. This is a deliberate, accepted trade of
  generated-code quality for uniformity and a much smaller compiler; it is
  the single strongest motivating case for starting the MIR optimizer.

- **A float argument to a variadic (`printf`-style) call reads garbage
  from whatever's left in `%al`.** Not this compiler's bug — Cranelift
  itself has no support for the x86-64 SysV vararg calling convention
  (`%al` must hold the caller's XMM-register count; confirmed nothing in
  `cranelift-codegen` handles this at all, and `rustc_codegen_cranelift`
  hit the same wall and just forbids the shape). Surfaces differently
  depending on register allocation: a function parameter forwarded
  straight into a variadic call (any `-O` level), an enum body-field
  projection (`-O1`+ only), or even a plain local in a large enough
  function (previously believed safe; it isn't, it was just small
  enough to not show it). [primitives.md](01-primitives.md)
- **No real C-ABI aggregate-passing convention** — structs/enums pass as
  flattened positional scalars, fine Omega-to-Omega, not safely callable
  from hand-written C expecting real struct-passing rules.
  [primitives.md](01-primitives.md)
- **Extern *data* declarations (a non-function `extern`) have no storage
  story** — fully resolved and type-checked like everything else, but
  codegen still has nothing sound to do with it (`todo!()` in
  `update_extern_decl`), since its storage genuinely lives in another
  translation unit. An ordinary top-level global (`ident: type;`, or a
  non-`comp` `ident := comp value;`) is *not* this gap anymore — both
  are fully implemented, including `mut`. [mir-and-codegen.md](16-mir-and-codegen.md),
  [compile-time-evaluation.md](19-compile-time-evaluation.md)
- **Assigning *into* a function parameter directly (no deref in between)
  is still `todo!()`** — taking a parameter's *address* is fixed (see
  [mir-and-codegen.md](16-mir-and-codegen.md)'s own "Fixed" note); direct
  assignment is a separate, still-unfixed code path. An explicit local
  copy works around it today. [mir-and-codegen.md](16-mir-and-codegen.md)
- ~~**A monomorphized *conform* method gets strong linkage**~~ — **fixed.**
  `linkage_for` decided weak vs. strong from the *function's* own `type_args`,
  which is empty for a conform method: the genericity lives in the target
  (`Self`), not the method. So two packages that each called `println$` — whose
  expansion instantiates `conform<W: Write> BufWriter<W> to Write` at
  `BufWriter<Stdout>` — failed to link with `multiple definition of
  _omg_…BufWriterNtBb_6StdoutE5Write5write…`, while the type's *inherent*
  methods and its vtable symbol were correctly weak.

  `ConformanceOwner::from_template` now carries the missing bit (set from whether
  `instantiate_conformance` was handed a substitution — only a template match
  produces one), and codegen gives a template instantiation `Preemptible`
  linkage while a directly-written concrete conform stays `Export`, so a
  genuine duplicate is still an error. Guarded by `just test-multi-print`,
  which links two printing packages and fails without the fix.

## Types

- **`*str` is not actually guaranteed valid UTF-8** — casting between
  `*str` and `*[]u8`/`*[]i8` is unsound in both directions, no validation.
  Deliberately deferred pending a `core`-provided validating conversion.
  [strings-casting-and-slices.md](11-strings-casting-and-slices.md)
- **`char`'s classifiers are ASCII-only, not Unicode.** `is_alphabetic`,
  `is_whitespace` and `to_ascii_*` cover the ASCII range and nothing beyond
  it; a `char` above `0x7F` is reported as neither alphabetic nor whitespace
  regardless of what Unicode says. Full classification needs property tables,
  which do not belong in a freestanding `core` without a decision about where
  that data lives and what it costs in code size. The names are deliberately
  honest about the `to_ascii_*` half; the `is_*` half is the one that could
  mislead. [primitives.md](01-primitives.md)
- **`char`'s validity is a supported path, not an enforced invariant.**
  `char::from_u32` rejects out-of-range values and UTF-16 surrogates, and the
  direct `<char>some_u32` cast stays refused — but a pointer reinterpretation
  (`*<*char>&some_u32`) still produces an arbitrary bit pattern as a `char`,
  from any package. This is accepted rather than fixed: closing it means
  restricting pointer casts, which contradicts the honest-address model. It
  is recorded because several comments would otherwise be tempted to claim a
  `char` is always valid — the true statement is that the supported path
  always produces a valid one. [primitives.md](01-primitives.md)
- **There is no `!` (logical-not) operator for `bool`.** `& | ^` are `bool`'s
  logical operators (non-short-circuiting, since `&&`/`||` do not exist
  either), and negation is written `if x { false } else { true }` — see
  `core::cmp`'s `not_equals`. Adding `!` is a real if small language feature:
  a new parser token plus a new `Expression`/`HirExpr`/`CheckedExpr`/`MirExpr`
  variant. [control-flow.md](03-control-flow.md)
- **`std::fmt`'s float output is fixed-precision, not round-trip** — six
  fractional digits, with a scientific fallback below `1e-6` and at or above
  `1e19` whose normalization loop (repeated multiply/divide by ten) is itself
  lossy. `nan`/`inf`/`-inf` are exact. A shortest-round-trip formatter
  (Ryu/Grisu-class) is deliberate future work, not a narrow fix here.
  [console-io.md](24-console-io.md)

## Conformance and specs (`conform` / `primitive`)

- **Calling an *inherited* spec method through the deriving spec's bound
  compiles but fails to link.** A conform block for a dependent spec supplies
  the whole flattened chain, and registers every method under the spec that
  was *conformed to* — but a call site resolves the method to the spec that
  *declared* it, and the two mangle differently. Minimal repro:

  ```
  same<T: Ord>(a: T, b: T) => bool { a.equals(b) }
  main() => i32 { if same(1, 1) { 0 } else { 1 } }
  ```

  `equals` is declared on `Eq`; `i32`'s `conform i32 to Ord { equals; … }`
  emits `…Xl3Ord6equals…`; the call emits a reference to `…Xl2Eq6equals…`,
  which nothing defines. Clean compile, `undefined reference` at link.

  A spec's *own* methods are unaffected (`compare`, `greater_than` link
  fine), so the workaround is to route through one of those — which is why
  `core::range`'s `RangeIterator::next` calls
  `compare(other).is_eq()` rather than the more readable `equals(other)`.
  This is a mangling/resolution mismatch, not a missing emission: the method
  exists, under the other name.


Every issue tracked here through plan 0005 is now fixed and verified; see
[specs.md](08-specs.md) for the resulting behaviour. What remains are two
deliberate limitations and one coverage gap.

- **A `spec T` return type on a *method* is rejected, not inferred**
  (`SpecStaticNotAllowedHere`). Only a plain, non-overloaded top-level
  function infers its return type from its own body, via
  `Driver::resolve_spec_return_function`'s phase inversion.

  Inferring it for a method is *reachable* but wrong: `collect_methods` would
  have to check the body during the signature phase, while the loop building
  the owning type's `functions` list is still running, so the body sees
  `Self`'s fields but none of its sibling methods — failing with
  `no field 'helper' on 'Zoo'` in either declaration order. That was
  implemented once and reverted: a partially-populated cell reaching user code
  is worse than the missing feature. Doing it properly means extending the
  same inversion to run *after* the owning aggregate's other method signatures
  are known, which is a phase change in `compute_aggregate`, not a local
  override. [specs.md](08-specs.md)

- **A variadic spec function is rejected at its declaration**
  (`VariadicSpecFunctionUnsatisfiable`). Omega has no variadic function
  *definitions* — only `extern` declarations may be variadic, for C interop —
  so neither a `conform` block nor a spec default can supply a matching body,
  and every implementor would otherwise fail with a bare
  `MissingSpecFunction` naming a function it has no syntax to write. The
  `is_variadic` plumbing behind it (HIR, `RawSpecFunctionSig`, the resolved
  `ResolvedFunctionType`) is complete; only this guard stands between it and
  working, and it should be lifted the day variadic definitions exist.

- **A generic parameter cannot be inferred from a slice argument.**
  `f<T>(x: *T)` called with a `*[]u8` reports "cannot infer type parameter
  'T' from this call's arguments", and `f<[?]u8>(...)` is not valid syntax
  either. Nothing to do with conformance — it reproduces with no spec or
  conform in the program — but it is what stops a slice conform from being
  reached through a generic bound: `Show::show(s)` works, `use_it<T: Show>(s)`
  does not. [generics](06-generics.md)

- **Coverage gap: which conform body a call selects is not unit-testable.**
  `compiler/omega-driver/tests/conform.rs` produces a `CompiledProgram` and
  cannot execute it, so
  `an_explicit_conform_wins_over_a_derived_dependency_entry` asserts only
  declaration-level facts. The bug it guards — a derived dependency entry
  shadowing a later explicit conform, so `Base::b` silently ran `Derived`'s
  body — emitted *both* bodies before and after the fix, making the emitted
  set non-discriminating. Closing this needs a compile-and-run harness, which
  only `just test-io`/`run-exec` provide today.


## Gaps and glue

- **No default-bodied `gap` function** — every gap function must
  currently be a bare requirement; a body is rejected outright
  ([gaps-and-glue.md](21-gaps-and-glue.md)).
- **No "override" or test-only glue concept** — a second `glue` for the
  same gap is always a hard error project-wide, with no way to shadow one
  intentionally. [gaps-and-glue.md](21-gaps-and-glue.md)
- **`MultipleGluesForGap` cannot point at the conflicting glue blocks.**
  The error is anchored at the *gap*'s declaration (correctly — neither
  glue is more at fault), and names each conflicting glue as
  `<module path>#<internal HirId>`, e.g. `plat#1, other#1`. Within a single
  module that degrades to `t#3, t#7`, which names nothing a reader can act
  on. The real fix is a secondary diagnostic label at each glue's own span,
  and those spans are in *different files* from the primary — the renderer
  only supports same-file secondary labels today (`Redeclaration`'s
  `previous: Option<Span>` is the only precedent). Resolving it means
  either cross-file labels in `omega-diagnostics`, or having
  `Driver::sweep_gaps` emit one additional `CompileError::Analysis` per
  glue site in that glue's own module. Left alone because the choice
  between those is a diagnostics-subsystem design decision, not a local fix.
- **`@suppress(unfilled_gap)` is unreachable.** Every warning's rendering
  ends with the generic "suppress this with `@suppress(<slug>)`" note, so
  `UnfilledGap` advertises it — but `gap` is a first-class declaration that
  takes no annotations at all, so following the advice is now a hard parse
  error. It never worked before either: `Driver::sweep_gaps` constructs the
  warning directly rather than going through `Analyzer::warn`, which is the
  only thing that consults `@suppress`. Fixing it means either giving
  `HirGapDef` an annotation list (which the gap/glue plan deliberately
  avoided, to keep anything downstream from branching on gap-level
  metadata) or teaching the whole-program sweeps to honour suppression and
  suppressing the note when a warning kind has no suppressible anchor.
  [gaps-and-glue.md](21-gaps-and-glue.md)

## Visibility

- **No re-export / `pub use`-equivalent.** Matches the language having no
  re-export concept at all today. [visibility.md](07-visibility.md)

## Macros

- **A node built from a macro expansion gets a composite span running from
  the call site to the definition site, and statement position makes it
  visible in ordinary diagnostics.** Every token keeps its own real
  originating span (deliberately — there is no render-to-text-and-relex
  round trip), so a node built from a mix of call-site argument tokens and
  definition-site body tokens spans both. `expand_expr` hides this for
  expression position by pinning the invocation's own call-site span back
  onto the outer node, but a statement-position expansion has no equivalent:
  the spliced statements and their expressions keep the composite spans the
  re-parse produced. `just build-exe` on `examples/dev/main.omg`
  demonstrates it — the `unused return value` warning for
  `call_each$(puts, ...)` at line 769 renders a label stretching to the
  macro's own definition at line 1495, ~700 lines of elided snippet for a
  one-line statement. Not fixed here because the honest fix is a single
  span policy shared by all three positions (item position has the same
  latent problem), which is a design decision rather than a local patch:
  either remap every span in an expansion to the call site (losing the
  ability to point inside a macro body at all) or give `Span` a notion of
  expansion provenance so the renderer can show both. Pinning only the
  top-level statement node would fix the demonstrated case while leaving
  every nested expression inside an expansion still wrong.
  [macros.md](12-macros.md)
- **`MAX_EXPANSIONS` does not actually prevent the stack overflow it
  documents.** `macros.rs`'s budget is spent one unit per invocation and
  reports `ExpansionLimitExceeded` at 256, but each expansion costs roughly
  twenty stack frames (the recursive-descent re-parse plus `expand_expr`'s
  own very large frame), so `macro a() => { a$() }` aborts on a stack
  overflow before the budget runs out on a 2 MiB thread stack — it only
  reports cleanly with `RUST_MIN_STACK` raised. Pre-existing: reproduced
  identically on the baseline commit with the old `a!()` syntax. Statement
  position adds a second recursion path (`expand_statements_invocation`)
  with the same shape. The fix is a *depth* limit rather than (or as well
  as) a total-expansion budget. [macros.md](12-macros.md)
- **Duplicate macro parameter names are silently accepted**, e.g.
  `macro m($a: expr, $a: expr)`; bindings are a `HashMap`, so the later
  parameter wins and the earlier one becomes unreferenceable. The same
  applies when a fixed parameter and the variadic share a name, where the
  variadic's `Many` binding shadows the fixed `One`. Pre-existing (the flat
  `Vec<MacroParam>` model had it too); the fix is one duplicate check in
  `parse_macro_signature` plus a `ParseErrorKind` variant.
  [macros.md](12-macros.md)
- **A repetition separator is not restricted to tokens that can survive
  substitution.** `parse_repetition` only rejects brackets and multi-token
  separators, so `$...($x){ ... }` or `$...($){ ... }` parses, emits the
  `$name`/`$` token literally, and fails much later with a confusing
  expansion-site parse error rather than at the definition. Low impact
  (nobody writes it deliberately), but the diagnostic points at the wrong
  place. [macros.md](12-macros.md)
- **Macro visibility is not transitive.** A module's macro environment is
  built from its *own* import statements and each target's *own* definitions;
  an imported module's imports are never followed. This matches the language
  having no re-export concept, and it is what keeps the pre-pass acyclic, but
  it means a package cannot curate a macro surface the way it can't curate an
  item surface. [macros.md](12-macros.md), [visibility.md](07-visibility.md)
- **Importing a macro leaves a spurious `unused import` warning.** Macro
  names are resolved and consumed by the pre-pass in `omega-driver`'s
  `Driver::macro_env`, entirely before HIR exists, so the ordinary
  import-usage tracking never observes the use and reports the import as
  dead. Every cross-package macro import warns today.
  [macros.md](12-macros.md), [visibility.md](07-visibility.md)

## Compiler internals

Shape problems in `omega-driver` and `omega-analyzer` that work today but each
need a breaking change to fix — full writeups in
[design-review.md](17-design-review.md#compiler-architecture).

- **Overloading needs a whole parallel item pipeline** (two extra caches,
  two extra sweeps, two extra resolver methods) purely because the item
  query key can't name one candidate of an overload group — which also
  makes generic overloads structurally impossible. Confirmed: a generic and
  non-generic overload of the same name (`f(x: i32)` / `f<T>(x: T)`) doesn't
  just get rejected, it fails with an opaque, rootless diagnostic
  (`ResolveError::ItemFailed` firing with no primary error ever shown) —
  likely `ensure_overload_signature` resolving a generic candidate's own
  signature with an empty substitution list.
- ~~**Two independent pending-spec-method queues**~~ — **fixed.** With
  conformance living only in the conform registry, an aggregate queues
  nothing, so the `ItemKey`-keyed queue was deleted outright rather than
  unified; `ConformanceEntry::pending` is the only one left.
- **A directory sharing its package root's name is skipped without saying
  so.** `fs_resolve::discover_tree`'s `skip` matches by name, not by kind, so
  `<root>/<basename>/` is swallowed along with the `<root>/<basename>.omg` it
  exists to de-duplicate. The *consequence* is now diagnosed —
  a package that ends up with no modules is `CompileError::EmptyPackage`,
  which names the expected root file and tells an old-layout package what to
  move (it previously panicked on `compile`'s "always includes at least the
  entry module" expectation). What remains is that nothing reports the skipped
  directory itself, so a package with both a root file *and* a same-named
  subdirectory still loses the subdirectory quietly. Full writeup in
  [modules and linkage](10-modules-and-linkage.md#known-gap-a-same-named-subdirectory-hides-itself-silently).
- **`ResolveError::Cycle` carries a chain it never populates** — it always
  prints one module, so the rendered message implies a cycle it never
  shows.
- **Module paths and item paths are the same untyped `Vec<Ident>`**, so
  nothing prevents confusing the two.
- **Diagnostic scoping for scanned (extern/`core`) modules is three ad-hoc
  lists** with four different outcomes and no stated policy.
- **A node's identity is threaded as a bare `(HirId, Span)` pair** through
  ~60 analyzer signatures, with nothing tying the two together.
- **`reveal`'s bypass must be re-activated by every operand position
  individually**, with no backstop — three positions have now been fixed
  one at a time.

## Design debt worth watching

- Every new `Expression`/`HirExpr`/`CheckedExpr` variant needs updates
  across up to five separate exhaustive matches spread over multiple
  crates (macro expansion, prelude re-exports, HIR lowering, defer-id
  collection, codegen emission) — the compiler catches every miss as a
  hard exhaustiveness error, so nothing is silently skipped, but budget
  for it when adding new expression forms.

## Diagnostics

- ~~**A method call's receiver, and any write through a projection, did not
  count as reads**~~ — **fixed.** `Context::mark_used` was reached from a
  single site (`analyze_expr`'s `HirExpr::Place` arm), which a *receiver*
  never goes through — it is analyzed as a place instead — and which a
  projected write (`*out = 5`, `s.v = 5`) does not reach either. So a
  parameter used only as a receiver, or only as an out-pointer, reported
  `UnusedParameter`: `write_bool(out: spec *mut Write, …)` used `out` twice
  and still warned, and `List::pop(*mut self, out: *mut T)` did too.
  Long-standing and not spec-object-specific — a concrete `d.get()` warned
  identically — but the stdio redesign made it unmissable, since every
  `write_*` helper uses its `out` parameter only as a receiver. Marked now in
  `resolve_callee` (receivers) and in `analyze_place` when the place has at
  least one projection (a projection must load its root to compute an
  address). A bare, projection-less `n = 5` is deliberately still a pure
  write, so `UnusedVariable` keeps firing on write-only bindings.

- **An unsatisfied generic-`conform` bound is reported once per conformance
  lookup, not once per declaration.** `Driver::instantiate_conformance` checks the
  template's own bounds before its memoization guard, and a failed check
  registers no entry, so every later `conformances_for_type` call for the same
  target re-runs the check and re-reports. `conform<T: W> Buf<T> to W` against
  a `Buf<NotW>` that is also coerced to `spec *mut W` prints the identical
  `SpecNotImplemented` twice. The anchor and wording are correct; only the
  count is wrong. The fix is either a per-`(conform id, target)` "already
  reported" set in `Conformances`, or general diagnostic de-duplication — both
  wider than the conform path itself. [specs.md](08-specs.md)

- **Blanket conform bodies are checked lazily.** Like existing generic
  conformance templates, `conform<T: Bound> T to Spec` is type-checked only
  once a concrete target satisfies its bound. An unused invalid body can
  therefore ship in a library until some consumer materializes it.

- **Latent blanket overlap is diagnosed at use, not declaration.** The
  compiler intentionally does not try to prove whether arbitrary spec bounds
  overlap. Two unrelated blankets become an `AmbiguousConformance` only when a
  concrete type satisfies both; this avoids rejecting declarations that can
  never apply together, at the cost of a downstream diagnostic.

- **A blanket emits a body for every type it is *materialized* against, not
  every type that calls it.** `Driver::materialize` runs whenever any
  conformance question is asked about a type — a bound check, a spec-qualified
  path, a `for..in` source — and `check_conformance_bodies` emits a body for
  every registered entry, with no reachability test. So in a program with
  `conform<T> T to Sum` where only `A` ever calls `Sum::sum`, but `B` and `C`
  are queried because they conform to some *other* spec, all three get a
  `Sum::sum` body in the object file. Measured, not theorised.

  Not a correctness or binary-size problem: codegen puts each function in its
  own section (`ObjectBuilder::per_function_section`) and every link uses
  `--gc-sections`, so the dead copies never reach the executable. It costs
  object size and compile time, proportional to (types queried × matching
  blankets), and unbounded blankets are the worst case since their bound
  filters nothing. The real fix is demand-driven conformance emission rather
  than registration-driven; that is a change to how `check_conformance_bodies`
  is scheduled, not a local tweak. [specs.md](08-specs.md)

- **A `*mut self` requirement against an rvalue receiver reports
  `NotMutablePointer`, and the invariant that made that correct no longer
  holds.** `Bump::bump(make())`, where `bump(*mut self)` and `make()` returns
  by value, is correctly *rejected* — the mutation would land in a temporary
  that is immediately discarded — but the message is `cannot mutate through
  an immutable pointer`, naming a pointer that does not appear in the source
  and that no added `mut` can fix.

  `Analyzer::require_mutable_place` (`analysis/places.rs`) picks between
  `NotMutableBinding` and `NotMutablePointer` by asking whether the checked
  place has a `Deref` projection, falling through to `NotMutablePointer` for
  any root that isn't an unqualified path. Its own doc comment states the
  assumption that justified that fallback: "a non-place root, e.g. a
  freshly-constructed value, is never itself the *cause* of immutability --
  something dereferenced along the way always is." **That is no longer
  true.** Spec-qualified calls now wrap a non-place receiver in
  `HirPlaceRoot::Expr` so it can be adapted at all (see
  `Analyzer::adapt_self_argument`), producing a place with a non-path root
  and *no* `Deref` projection — the exact shape the assumption excluded.

  Pre-existing in the sense that a receiver-position call on an rvalue
  reaches the same arm, but newly *reachable* in ordinary code: a
  spec-qualified call is the normal way to invoke a conforming method, and its
  receiver is an ordinary argument expression that anyone may write as a call
  or a literal. The fix is a third `AnalysisErrorKind` for the not-a-place
  case — "`*mut self` needs a place to mutate; bind the value to a `mut`
  local first" — selected in `require_mutable_place` before the
  `through_pointer` test, plus a correction to that doc comment.
  [specs.md](08-specs.md)
- **Type-level capture remains possible in macro-generated declarations.**
  Generic parameters and `Self` intentionally ignore macro origin, because
  they are substitution-bound rather than lexical bindings. A generated type
  parameter can therefore still capture a same-named type from a substituted
  argument. There is no in-tree instance; partitioning these bindings would
  break the `Self` uses in the primitive conformance macros.
  [macros.md](12-macros.md)
- **Macro-authored unused locals are not linted.** Expansion spans are anchored
  at the invocation and carry no source-file identity, so reporting the lint
  would misleadingly blame the caller. Locals introduced by a macro are
  intentionally excluded from `unused variable`; caller-origin arguments are
  still use-tracked normally.
  [macros.md](12-macros.md)

## Control flow

- **A bare `return;` is a parse error**, so a `void` function cannot return
  early at all — `expected an expression, found ';'`. Every early exit in a
  `void` body has to be restructured around a sentinel flag, which
  old fixed-buffer I/O helpers had to do; the current `std::io::read_line`
  loops with a sentinel flag instead.
  [control-flow.md](03-control-flow.md)
