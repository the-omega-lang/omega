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

- **`Type` equality compares parameter *names*.** Inside `FunctionType`,
  `params: Vec<Param>` and `Param`'s hand-written `PartialEq` compares
  `ident` as well as `r#type`, so `(a: i32) => void` and `(b: i32) => void`
  compare unequal. Harmless today — the analyzer compares `ResolvedType`,
  never raw `Type` — but latent if raw `Type` equality ever becomes
  load-bearing. `Param` already drops spans and `origin` from equality,
  following `Path`'s precedent; whether the *name* belongs in a function
  type's identity is the open question.
  [parsing-and-hir.md](15-parsing-and-hir.md)


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

- ~~**There is no `!` (logical-not) operator for `bool`**~~ — **fixed.**
  `bool` now has `!`, `&&` and `||` alongside the existing `&`/`|`/`^`. It
  turned out cheaper than the estimate here: all three desugar during
  analysis into forms the language already had (`!x` to `x ^ true`, `&&`/`||`
  to the `if`-expressions the idiom already used), so no `CheckedExpr`,
  `MirExpr` or codegen variant was needed — only a token, a grammar tier and
  an HIR node. [control-flow.md](03-control-flow.md)

- **`std::fmt`'s float output is fixed-precision, not round-trip** — six
  fractional digits, with a scientific fallback below `1e-6` and at or above
  `1e19` whose normalization loop (repeated multiply/divide by ten) is itself
  lossy. `nan`/`inf`/`-inf` are exact. A shortest-round-trip formatter
  (Ryu/Grisu-class) is deliberate future work, not a narrow fix here.
  [console-io.md](24-console-io.md)

## Conformance and specs (`conform` / `primitive`)

Every issue tracked here through plan 0014 is now fixed and verified; see
[specs.md](08-specs.md) for the resulting behaviour — goal-directed
conformance proving (blanket chains resolve in any declaration order, and a
genuine cycle prints the chain that closes it), alias-transparent bounds
(`T: AB` and `T: A + B` are interchangeable in precedence and in bound
contexts), return-type-driven generic inference (a generic named only in a
call's expected type is inferred from it, for free functions and generic
statics alike), and the two diagnostics that no longer name constructs the
author never wrote (`MutateTemporary` for a `*mut self` call on a temporary,
`GenericParamFromFatPointer` for a thin `*T` against a slice). What remains:

- **Blanket conform bodies are checked lazily**, and a blanket emits a body
  for every type it is *materialized* against, not every type that calls it.
  Goal-directed proving has reduced the materialization set — a type is only
  ever swept for the specs something actually asked about, so an unrelated
  blanket is no longer instantiated just because the type was queried for
  some other spec — but the real fix is demand-driven conformance emission
  rather than registration-driven; that is a change to how
  `check_conformance_bodies` is scheduled, not a local tweak. Not a
  correctness or binary-size problem: codegen puts each function in its own
  section and every link uses `--gc-sections`, so dead copies never reach
  the executable. [specs.md](08-specs.md)

  **Measured**: `target/core.o` went from 226 defined symbols to 204 across
  plan 0014 — the 22 removed are `Eq::equals`/`Eq::not_equals` for the 11
  scalars, bodies `core::cmp`'s `conform<T: Ord> T to Eq` used to emit for
  every scalar the compilation happened to query, and which `core` itself
  never calls. This is a *reduction*, not a regression: a downstream package
  that does call one materializes the blanket itself and emits its own weak
  copy, which links and runs (verified). Anyone re-running an
  `nm --defined-only` comparison against a pre-0014 object file should
  expect this difference and no other; `target/std.o` is unchanged at 91.

- **Latent blanket overlap is diagnosed at use, not declaration.** The
  compiler intentionally does not try to prove whether arbitrary spec bounds
  overlap. Two unrelated blankets become an `AmbiguousConformance` only when
  a concrete type satisfies both; this avoids rejecting declarations that
  can never apply together, at the cost of a downstream diagnostic.
  [specs.md](08-specs.md)

- **Design note: definition-site `spec T` return types are removed.**
  `make() => spec Animal { ... }` is now `SpecStaticNotAllowedHere` on a
  free function and a method alike, deliberately — the syntax promises "some
  unknown type implementing XYZ", which is true of a spec *declaration*
  (each implementor answers differently) and false at a definition site
  (one body, one type, known to its author), and its only benefit was
  hiding a name. The removed machinery was a phase inversion (body analysis
  during the signature phase); the rule is now uniform: a return type is
  either written concretely, or chosen by the *caller* through an ordinary
  generic parameter (`f<T: Animal>() => T`). The spec-declaration position
  (`to_iterator(*self) => spec Iterator<T>`) and the parameter position
  (unchanged sugar) both stay — see [specs.md](08-specs.md). Reopening this
  would need a compiler that can afford body analysis during the signature
  phase in every compilation module; until then the workarounds are to name
  the concrete type or take a bound generic parameter.
  [specs.md](08-specs.md)

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

- **Analyzer-synthesized nodes reuse their parent's `HirId`, and that is
  only safe because nothing reads it.** Every desugaring that mints
  `CheckedExprNode`s — `analyze_incr_decr`, `analyze_compound_assign`, and
  the newer `analyze_not`/`analyze_logical` — stamps the *parent* node's
  `HirId` onto each synthesized child, so a single lowered expression can
  yield several checked nodes sharing one id. Verified safe today: nothing
  in `omega-mir` or `omega-codegen` reads `CheckedExprNode::id`, and every
  `HirId`-keyed map in the compiler is keyed on a *declaration* id, never an
  expression id. **This is a live constraint, not an observation** — the
  moment anything keys a map on an expression's id, or a diagnostic dedupes
  by it, these desugarings start colliding silently. Worth settling
  deliberately during the `omega-analyzer` pass: either mint fresh ids in
  the analyzer, or state the invariant where it can be seen.
  [parsing-and-hir.md](15-parsing-and-hir.md)

- **Macro expansion still rebuilds the whole tree by value to recurse.**
  `omega_parser::macros::Expander::expand_expr` was given a context struct
  (so the `(defs, budget, state)` triple stopped being threaded by hand) and
  had its per-arm unbox dance collapsed, but the traversal itself is
  unchanged: every arm still reconstructs its node field-by-field purely to
  descend into children, and `expand_struct_def`/`expand_union_def` remain
  two character-identical functions. The intended shape was an in-place
  `&mut` walk over a `children_mut` iterator, leaving explicit arms only for
  the block-bearing variants (`Codeblock`, `If`, `Match`, `Slice`'s range).
  Not done here because it is a genuine design change to the compiler's
  single highest-risk file, and because `children_mut` would have exactly
  one consumer — worth deciding on its own rather than as the tail of a
  refactor. The cost of leaving it is ~70 lines of reconstruction that a new
  `Expression` variant must be added to in two places instead of one.
  [parsing-and-hir.md](15-parsing-and-hir.md)

- **Three of the spans added by the span-ownership pass have no reader.**
  `FunctionDefinitionStmt`/`SpecFunctionStmt`/`HirFunctionDef`/
  `RawSpecFunctionSig::signature_span` is set by the parser and copied
  through four structs across three crates, and nothing ever reads it; the
  same is true of `CodeblockExpr::span` and the `HirBlock::span` it feeds.
  They were added because the span-ownership rule (*a construct that can be
  the subject of a diagnostic owns its span*) says they should exist, not
  because a diagnostic needed them — the two that fixed real defects
  (`name_span`, `return_type_span`) do have readers. Left in place rather
  than deleted because the anchors that would use them are already written
  down (see the three widened anchors under **Diagnostics**), so removing
  them now would only mean re-adding them. **Decision needed:** either
  narrow those anchors and consume these spans, or drop them and stop
  carrying a field the pipeline does not use.
  [parsing-and-hir.md](15-parsing-and-hir.md)

- **`Display for ParseErrorKind` builds and discards a whole `Diagnostic`.**
  Collapsing each parse error's definition to one site made
  `ParseError::to_diagnostic` the only place that knows an error's text, and
  `Display` now reads its headline back from there — which means formatting
  an error clones the kind, allocates its labels and footers, and throws all
  but `message` away. Correct, and only on the macro-expansion error path
  where it is rare, but it is a real cost paid for a wording guarantee.
  A `message_only` split inside `to_diagnostic` would remove it without
  reintroducing a second definition site.
  [parsing-and-hir.md](15-parsing-and-hir.md)


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

- **Parameters and aggregate fields are the same type at all three layers.**
  `omega_hir::HirParam` carries a `visibility` that is meaningful only in
  the field role and inert for a parameter; `omega_analyzer::CheckedParam`
  serves both roles and carries no visibility at all; field visibility
  travels separately as `Vec<(Ident, ResolvedType, Visibility)>` on
  `ResolvedStructType`/`ResolvedUnionType`/`ResolvedEnumType`. Three
  representations of one fact across two crates. Deliberately **not** split
  during the parser/HIR refactor: introducing an `HirField` alone would
  create a distinction that dies one layer later, when
  `analyze_struct_fields` re-merges both roles into `CheckedParam`. Fix the
  whole chain as one unit in the `omega-analyzer` pass.
  [parsing-and-hir.md](15-parsing-and-hir.md)
- **The contextual-keyword set grows with every feature, with no promotion
  policy.** Eighteen words are now position-dependent keywords
  (`parser::contextual`). Each one is a place where a lookahead can commit
  too early and silently stop the word being usable as a name — three had
  already done exactly that. The registry plus its generated test make the
  set visible and guarded, but there is no stated rule for when a word
  should graduate to a real reserved keyword instead.
  [parsing-and-hir.md](15-parsing-and-hir.md)
- **`CheckedSlice` still flattens a range end the way `HirRange` used to.**
  `omega_hir::HirRange` now carries a three-way `HirRangeEnd`
  (`Inclusive`/`Exclusive`/`Open`), so "an inclusive range with no end" is
  unrepresentable in the HIR. `omega_analyzer::checked::CheckedSlice` still
  carries `end: Option<CheckedExprNode>` plus `inclusive: bool` one layer
  down, which has the same spare state — the analyzer just never builds it.
  Not fixed with `HirRangeEnd` because the change reaches codegen's slice
  emission in both backends, which is out of scope for a parser/HIR pass.
  Fix with `omega-analyzer`'s own refactor.
  [parsing-and-hir.md](15-parsing-and-hir.md)

- **Omega's calling convention is not the platform C ABI.** The largest
  piece of deliberate debt in the compiler, and it was *deliberately
  preserved unchanged* when the LLVM backend landed — mirrored rather than
  fixed, so that both backends agree with each other. Two facts, both in
  `omega-codegen/src/abi.rs`:

  1. **Aggregates are flattened into their scalar leaves**, each leaf
     becoming its own parameter. C's SysV instead classifies a struct into
     eightbytes: `struct { i32 a; i32 b; }` is *one* register under C, and
     *two* parameters here.
  2. **The return rule is `leaves > 2` → `sret`**, justified in its own doc
     comment by "x86_64 SysV has exactly two integer return registers
     (rax/rdx)". That is an x86_64 fact, and it is now applied unchanged to
     **every** architecture — aarch64 (x0/x1, plus x8 as the indirect
     result register) and riscv (a0/a1) included.

  C's variadic default-argument promotion *is* implemented correctly
  (`abi::variadic_promotion`), so variadic C interop is unaffected.

  What this does and does not break: Omega-to-Omega calls are correct on
  every backend and target, because both backends read the same
  `AbiSignature` and therefore agree exactly — that is what lets a
  Cranelift `core.o` link against an LLVM `main.o` (`just test-mixed`).
  Only the **C boundary** is wrong, and only for aggregates passed or
  returned **by value**. Scalars and pointers — all Omega's C interop uses
  today — are correct.

  To keep it that way rather than waiting for someone to discover it,
  aggregate-by-value across an `extern` boundary is a **hard error**
  (`AnalysisErrorKind::ExternAggregateByValue`) pointing back at this
  entry. One `if` to delete once a real per-target C ABI exists; until
  then it turns a silent miscompile into a compile error.

  Fixing it properly means per-target ABI classification in `abi.rs`
  (eightbyte classification for SysV, AAPCS for aarch64, ...) plus a
  decision on whether Omega's *own* convention should follow the
  platform's or stay deliberately its own with `extern` as the sole
  translation point. That decision has not been made.

- **`target/debug/omgc` is one path for two different builds.** `cargo
  build` and `cargo build --features llvm` write the same binary, so
  whichever ran last wins — a plain `cargo test` leaves an `omgc` that
  rejects `--backend=llvm`. Every `just` recipe needing LLVM depends on
  `build-llvm` first, so the gates are unaffected; it only bites when
  running `omgc` by hand.

- **`@layout(align = n)` is not yet a real address guarantee.**
  `layout::type_alignment` reports a type's *declared* `@layout(align)` and
  nothing else — it never propagates through a containing type. So for

  ```
  @layout(align = 16) struct Inner { v: i64; }
  struct Outer { pad: u8; inner: Inner; }
  ```

  `Outer` has alignment 1, `layout_fields` places `inner` at offset 16
  *within* `Outer`, and `Outer` itself is placed at whatever unaligned
  offset it lands on — so `inner`'s absolute address is aligned only by
  luck. Two consequences, both reachable today:

  1. `MirExpr::StructLiteral` concatenates only the *fields'* leaves, while
     `layout::leaves_of` includes the interior padding leaves an
     `@layout(align)` field forces. The whole-value write path and the
     byte-offset read path therefore disagree about where `inner` is, and
     the two backends give two *different* wrong answers. This predates the
     LLVM backend (Cranelift is equally wrong) and is a layout-model bug,
     not a codegen one.
  2. `MirPlace::align` is derived from `type_alignment`, and the LLVM
     backend turns it into a real `align` on every load and store.
     `llvm::place::offset_align` weakens the claim by the access's own byte
     offset, so nothing is over-claimed *relative to the place's base* —
     but the base itself can still be over-claimed when reached through a
     pointer (`p: *Inner` deref claims 16), because of the propagation gap
     above. Cranelift never claimed anything, so it cannot be miscompiled
     by this; the LLVM backend can, at `-O2`/`-O3`.

  Resolving it means deciding what `@layout(align = n)` actually promises:
  making `type_alignment` the max of a type's own declared alignment and
  its fields' (so a container inherits its members' requirement, as C and
  Rust both do), making `leaves_of`'s padding leaves reach every value
  construction path, and giving the language an aligned-allocation story
  for anything reached through a pointer. Until then, `@layout(align)` is
  usable for *relative* field placement and not as an address guarantee.
  No gate covers `@layout(align)` at all today, which is why this was not
  caught earlier.

- **Nothing gates a 32-bit target end to end.** Phase A of the
  LLVM-backend work made every width-sensitive analyzer question read the
  real target width, and `riscv32-none`/`thumbv7em-none` objects do emit —
  but the coverage stops at object emission plus two analyzer-level
  assertions (`comp sizeof<usize> == 4`, and a `usize` literal above
  `u32::MAX` being rejected). Nothing links or *runs* a 32-bit image, so
  32-bit codegen is proved only by inspection. A residual hardcode in
  `ResolvedType::cast_class`'s pointer arm survived Phase A for exactly
  this reason and was found only by reading the emitted IR by hand.
  Closing it needs a 32-bit runner (qemu-user, or a freestanding image
  plus a linker script, which needs an entry convention Omega has not
  defined — `Os::None` stops at "emits a correct `.o`" on purpose).

- Every new `Expression`/`HirExpr`/`CheckedExpr` variant needs updates
  across up to five separate exhaustive matches spread over multiple
  crates (macro expansion, prelude re-exports, HIR lowering, defer-id
  collection, codegen emission) — the compiler catches every miss as a
  hard exhaustiveness error, so nothing is silently skipped, but budget
  for it when adding new expression forms.

## Diagnostics

- **No error codes, and no machine-applicable suggestions.** `Diagnostic`
  carries a message, labels, and `note:`/`help:` footers — there is no
  `E0308`-style stable code to look up or search for, and no structured
  "replace this span with this text" a tool could apply. Both are additive
  later, but every error site is a place that would need revisiting, so the
  shape is cheaper to decide early than late.
  [parsing-and-hir.md](15-parsing-and-hir.md)
- **A dangling annotation at end of file is silently dropped.** `@inline`
  with no item after it reports only `expected a top-level item, found end
  of input`; the annotation itself vanishes with no mention.
  [annotations.md](09-annotations.md)
- **Three anchors still point at more than the thing they are about.** The
  span-ownership pass fixed every *member*-level diagnostic (a duplicate
  field, method or spec function, and a return-type mismatch, all now
  underline the name or the declared type). Three sites elsewhere still
  widen: `omega_driver::Driver::check_overload_duplicates` anchors a
  duplicate **top-level** function at `item_id_span`, which is the whole
  definition including its body; and `parse_gap_def`/`parse_glue_def` anchor
  `GapFunctionSelf`/`GapFunctionBody`/`GlueFunctionShape` at
  `Parser::last_span()`, the member's closing brace, rather than at the
  offending name. All three now have a real span available
  (`HirFunctionDef::name_span`, `SpecFunctionStmt::name_span`); none was
  changed here because none was part of the reproduced defect.
  [parsing-and-hir.md](15-parsing-and-hir.md)


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

- **An alias bound and its inline spelling are not interchangeable in blanket
  precedence.** `conform<T: AB> T to X` and `conform<T: A + B> T to X`, where
  `spec AB = A + B;`, describe the same bound set but compare as incomparable,
  so a type satisfying both gets `AmbiguousConformance` rather than a duplicate
  diagnosis. Alias members are not expanded before the subset comparison.
  Conservative -- it errors rather than selecting silently -- but it does
  contradict the mental model that an alias is only a name for its members.
  The same non-expansion applies to the derivation subset test. [specs.md](08-specs.md)

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

- **`&&`/`||` reject a `never`-typed operand, but the `if` form they desugar
  to accepts one.** `flag && exit(1)` fails with `'&&' requires 'bool'
  operands, found 'never'`, while the equivalent
  `if flag { exit(1) } else { false }` compiles — so the operator is
  strictly narrower than the desugaring it produces. This follows
  `analyze_if`'s existing rule for a condition rather than being new, and
  diverging-in-one-branch is rare in practice, but it is an inconsistency
  between two spellings the docs present as equivalent.
  [control-flow.md](03-control-flow.md)


- **`bool` now has two spellings for each connective, and both are
  supported.** `a & b` and `a && b` differ only in whether `b` is evaluated;
  same for `|` and `||`. This is what C, C++ and Rust all do and what
  programmers expect, but it is still two mechanisms for one concept — the
  cleaner endpoint would be `&&`/`||`/`!` on `bool` and `&`/`|`/`^`/`~`
  reserved for integers. That is a breaking change to any `core`/`std` code
  using `&`/`|` on `bool`, so it was **not** taken unilaterally.
  **Decision needed:** keep both, or drop `&`/`|`/`^` on `bool`.
  [control-flow.md](03-control-flow.md)
- **Chained comparison is permanently a syntax error.** `a < b < c` now
  reports `comparison operators are non-associative` (it previously
  surfaced as a confusing `expected ';'`), matching Rust. Python chains it
  instead. **Decision needed:** is rejection the permanent answer, or should
  chaining eventually mean the conjunction?
  [control-flow.md](03-control-flow.md)
- **`&&` took a spelling that already meant something.** Adding the `&&`
  token silently changed the meaning of `a&&b` written without spaces: it
  used to lex as `&` `&` and mean "bitwise-and `a` with the address of `b`"
  — a program that compiles (an integer and a pointer both coerce for `&`,
  see [primitives](01-primitives.md)) — and now parses as the logical
  connective and fails type checking. `a & &b` with the space is unaffected,
  and `||` has no such collision because `|` is infix-only. This was
  accepted rather than designed: the same trade C and C++ make. **Decision
  needed:** leave it (and say so in the docs), or require whitespace around
  binary `&` so the two readings can never be confused.
  [control-flow.md](03-control-flow.md)
- **`comp <` and `reveal <` are always the operator, never a comparison.**
  Both are contextual keywords, so `comp`/`reveal` are legal variable names,
  and both commit to the prefix-operator reading as soon as something that
  could be an operand follows. A leading `<` can begin a cast
  (`comp <usize>N`, which has always been valid), so it must count as an
  operand — which means a *variable* named `comp` can never be the left side
  of a `<` comparison. No single-token lookahead separates the two readings.
  **Decision needed:** accept the asymmetry, promote these two words to real
  keywords, or give casts a spelling that does not start with `<`.
  [parsing-and-hir.md](15-parsing-and-hir.md)

- **A bare `return;` is a parse error**, so a `void` function cannot return
  early at all — `expected an expression, found ';'`. Every early exit in a
  `void` body has to be restructured around a sentinel flag, which
  old fixed-buffer I/O helpers had to do; the current `std::io::read_line`
  loops with a sentinel flag instead.
  [control-flow.md](03-control-flow.md)
