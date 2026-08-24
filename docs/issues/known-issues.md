# Known issues

Concrete current compiler/library bugs and unsupported cases. Resolved issues are removed from this file; history remains in git and `docs/plan/`. Language semantics are normative in `docs/language/`, so an implementation deviation recorded here does not redefine the language.

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
  struct into registers. LLVM's own optimizer does not do this for us
  without that MIR-level work first — it collapses the equivalent
  hand-written Rust because that code never had a cursor struct routed
  through an `Option<T>` match in the first place. This is a deliberate,
  accepted trade of generated-code quality for uniformity and a much
  smaller compiler; it is the single strongest motivating case for
  starting the MIR optimizer.

- **No real C-ABI aggregate-passing convention** — structs/enums pass as
  flattened positional scalars, fine Omega-to-Omega, not safely callable
  from hand-written C expecting real struct-passing rules.
  [primitives.md](../language/types-and-primitives.md)

- **A generic direct `foreign` function is rejected rather than
  monomorphized.** `foreign(cc) name<T>(...) => T;`/`{ ... }` parses and
  carries its generics through HIR, but `collect_foreign_function_signature`
  rejects any non-empty generics list outright
  (`AnalysisErrorKind::GenericForeignFunctionUnsupported`) instead of
  instantiating a signature/mangled symbol per type argument the way an
  ordinary generic function does. Wiring real instantiation through
  `ItemKey`/the driver's generic-instantiation cache for foreign items was
  deliberately deferred rather than half-implemented.
  [foreign-function-interface.md](../language/foreign-function-interface.md)

- **`defer` can currently slip into a repeatedly-evaluated loop condition or
  C-style `for` post expression through a nested codeblock.** Analysis only
  enters the loop scope while checking the loop *body*, so the existing
  `DeferInsideLoopNotSupported` restriction does not cover those header
  expressions. MIR intentionally gives each syntactic defer one flag/body pair,
  which is only sound when that defer site can be activated at most once; a
  repeated header evaluation violates that cardinality assumption. This should
  be fixed in semantic analysis by checking repeated loop expressions under the
  same defer-forbidden context as the body (or, as a larger language change, by
  defining and implementing per-iteration defer instances). It is not patched
  here because that policy belongs to analyzer/control-flow semantics rather
  than MIR lowering.
  [mir-and-codegen.md](../architecture/mir-and-codegen.md)

## Types

- **Ordinary indexing does not validate the index expression type during semantic
  analysis.** `project_index` analyzes `container[index]` with no expected type
  and records the resulting expression directly in `CheckedProjection::Index`.
  Codegen later assumes that expression is an integer scalar (LLVM converts it
  with `into_int_value()`), so a non-integer index can survive checking and
  fail inside codegen instead of producing a source diagnostic. The index
  domain/coercion policy belongs in `omega-analyzer` and should be made
  explicit there before MIR; this refactor intentionally does not add a
  codegen-side type workaround.
  [strings-casts-arrays-and-slices.md](../language/strings-casts-arrays-and-slices.md)

- **Function-type equality compares parameter *names*.** Inside
  `FunctionType`, `params: Vec<Param>` and `Param`'s hand-written `PartialEq`
  compares `ident` as well as `r#type`, and `ResolvedFunctionType`'s derived
  equality does the same, so `(a: i32) => void` and `(b: i32) => void` are
  different types and one cannot be assigned to the other. `Param` already
  drops spans and `origin` from equality, following `Path`'s precedent;
  whether the *name* belongs in a function type's identity is the open
  question. `ResolvedFunctionType::accepts` carves out exactly one exception
  today: an *unnamed* parameter, which only the compiler produces (the
  receiver of an unbound member function value), matches any name in its
  position.
  [parsing-and-hir.md](../architecture/parsing-and-hir.md)

- **A function type cannot leave its parameters unnamed.** `parse_param_decls`
  requires `name: Type` for every parameter, so `(i32, *u8) => bool` is a
  parse error and only `(a: i32, b: *u8) => bool` is accepted. Several
  normative examples predate this and still use the unnamed spelling —
  `functions.md`'s "Function types, calling conventions, and variadics",
  `foreign-function-interface.md`, and `grammar.md`'s anonymous-enum member
  example. Deciding whether unnamed parameters are part of the language is
  the same open question as the entry above; the docs and the parser should
  be reconciled in one direction once it is answered.
  [functions.md](../language/functions.md)


- **`*str` is not actually guaranteed valid UTF-8** — casting between
  `*str` and `*[]u8`/`*[]i8` is unsound in both directions, no validation.
  Deliberately deferred pending a `core`-provided validating conversion.
  [strings-casting-and-slices.md](../language/strings-casts-arrays-and-slices.md)

- **`char`'s classifiers are ASCII-only, not Unicode.** `is_alphabetic`,
  `is_whitespace` and `to_ascii_*` cover the ASCII range and nothing beyond
  it; a `char` above `0x7F` is reported as neither alphabetic nor whitespace
  regardless of what Unicode says. Full classification needs property tables,
  which do not belong in a freestanding `core` without a decision about where
  that data lives and what it costs in code size. The names are deliberately
  honest about the `to_ascii_*` half; the `is_*` half is the one that could
  mislead. [primitives.md](../language/types-and-primitives.md)

- **`char`'s validity is a supported path, not an enforced invariant.**
  `char::from_u32` rejects out-of-range values and UTF-16 surrogates, and the
  direct `<char>some_u32` cast stays refused — but a pointer reinterpretation
  (`*<*char>&some_u32`) still produces an arbitrary bit pattern as a `char`,
  from any package. This is accepted rather than fixed: closing it means
  restricting pointer casts, which contradicts the honest-address model. It
  is recorded because several comments would otherwise be tempted to claim a
  `char` is always valid — the true statement is that the supported path
  always produces a valid one. [primitives.md](../language/types-and-primitives.md)

- **`std::fmt`'s float output is fixed-precision, not round-trip** — six
  fractional digits, with a scientific fallback below `1e-6` and at or above
  `1e19` whose normalization loop (repeated multiply/divide by ten) is itself
  lossy. `nan`/`inf`/`-inf` are exact. A shortest-round-trip formatter
  (Ryu/Grisu-class) is deliberate future work, not a narrow fix here.
  [console-io.md](../guide/console-io.md)

## Conformance and specs (`conform` / `primitive`)

Remaining known conformance/spec issues:

- **Conformance-method and vtable linker identity currently omits the spec's
  module path.** `ConformanceOwner` retains `spec_module_path`, but
  `omega-mir::mangle::conformance_method_symbol` and `vtable_symbol` currently
  receive only the spec name + concrete spec arguments. `ExternFunctionKind::
  Conform` also drops the module path before external references reach MIR. Two
  distinct specs with the same name in different modules can therefore produce
  the same conformance-method/vtable identity for the same target and signature.
  This is deliberately not patched as a local refactor: adding the missing path
  changes emitted linker names and is therefore an ABI/separate-compilation
  migration. The fix must thread full spec identity through checked extern
  references, conformance definitions, vtable construction, codegen, and
  cross-package separate-compilation linkage tests in one change.
  [symbol-mangling.md](../architecture/symbol-mangling.md)

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
  the executable. [specs.md](../language/specs-and-conformance.md)


- **Latent blanket overlap is diagnosed at use, not declaration.** The
  compiler intentionally does not try to prove whether arbitrary spec bounds
  overlap. Two unrelated blankets become an `AmbiguousConformance` only when
  a concrete type satisfies both; this avoids rejecting declarations that
  can never apply together, at the cost of a downstream diagnostic.
  [specs.md](../language/specs-and-conformance.md)

## Gaps and glue

- **No default-bodied `gap` function** — every gap function must
  currently be a bare requirement; a body is rejected outright
  ([gaps-and-glue.md](../language/gaps-and-glue.md)).

- **No "override" or test-only glue concept** — a second `glue` for the
  same gap is always a hard error project-wide, with no way to shadow one
  intentionally. [gaps-and-glue.md](../language/gaps-and-glue.md)

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

## Macros

- **`MAX_EXPANSIONS` does not actually prevent the stack overflow it
  documents.** `macros/expander.rs`'s budget is spent one unit per invocation and
  reports `ExpansionLimitExceeded` at 256, but each expansion costs roughly
  twenty stack frames (the recursive-descent re-parse plus `expand_expr`'s
  own very large frame), so `macro a() => { a$() }` aborts on a stack
  overflow before the budget runs out on a 2 MiB thread stack — it only
  reports cleanly with `RUST_MIN_STACK` raised. Pre-existing: reproduced
  identically on the baseline commit with the old `a!()` syntax. Statement
  position adds a second recursion path (`expand_statements_invocation`)
  with the same shape. The fix is a *depth* limit rather than (or as well
  as) a total-expansion budget. [macros.md](../language/macros.md)

- **A repetition separator is not restricted to tokens that can survive
  substitution.** `parse_repetition` only rejects brackets and multi-token
  separators, so `$...($x){ ... }` or `$...($){ ... }` parses, emits the
  `$name`/`$` token literally, and fails much later with a confusing
  expansion-site parse error rather than at the definition. Low impact
  (nobody writes it deliberately), but the diagnostic points at the wrong
  place. [macros.md](../language/macros.md)

- **Macro visibility is not transitive through imports.** A module's macro
  environment is built from its *own* import statements and each target's
  *own* definitions; an imported module's imports are never followed. This is
  what keeps the pre-pass acyclic. It means a package cannot curate a macro
  surface by chaining plain imports the way it can't curate an item surface
  that way either — the deliberate mechanism for that, for macros exactly as
  for items, is a macro `alias` (see [`aliases.md`](../language/aliases.md)).
  [macros.md](../language/macros.md), [visibility.md](../language/visibility.md)

- **Importing a macro leaves a spurious `unused import` warning.** Macro
  names are resolved and consumed by the pre-pass in `omega-driver`'s
  `Driver::macro_env`, entirely before HIR exists, so the ordinary
  import-usage tracking never observes the use and reports the import as
  dead. Every cross-package macro import warns today.
  [macros.md](../language/macros.md), [visibility.md](../language/visibility.md)

## Compiler internals

- **Macro expansion still rebuilds the whole tree by value to recurse.**
  Expansion is now isolated in `macros/expander.rs`, and lookup clones only
  the requested macro definition instead of a whole environment, but the AST
  traversal itself still reconstructs nodes field-by-field purely to descend
  into children. `expand_struct_def`/`expand_union_def` also remain nearly
  identical. A shared AST fold/walk abstraction could make new expression or
  item variants harder to forget, but that would be a frontend-wide traversal
  design rather than a safe local cleanup. Decide it deliberately instead of
  growing a one-off visitor solely for macro expansion.
  [parsing-and-hir.md](../architecture/parsing-and-hir.md)

- **`HirBlock::span` is still carried without a downstream reader.**
  The frontend refactor now consumes `FunctionDefinitionStmt::signature_span`
  together with `CodeblockExpr::span` to give each lowered function its own
  precise span, so those parser fields are no longer dead metadata. The
  resulting `HirBlock::span`, however, is still copied through HIR and ignored
  by semantic analysis. Keep it only if blocks are expected to become direct
  diagnostic subjects; otherwise remove that field in a future HIR-shape
  cleanup instead of carrying location data with no consumer.
  [parsing-and-hir.md](../architecture/parsing-and-hir.md)

- **`Display for ParseErrorKind` builds and discards a whole `Diagnostic`.**
  Collapsing each parse error's definition to one site made
  `ParseError::to_diagnostic` the only place that knows an error's text, and
  `Display` now reads its headline back from there — which means formatting
  an error clones the kind, allocates its labels and footers, and throws all
  but `message` away. Correct, and only on the macro-expansion error path
  where it is rare, but it is a real cost paid for a wording guarantee.
  A `message_only` split inside `to_diagnostic` would remove it without
  reintroducing a second definition site.
  [parsing-and-hir.md](../architecture/parsing-and-hir.md)


Shape problems in `omega-driver` and `omega-analyzer` that still need a deliberate design change — full writeups in [design-debt.md](design-debt.md).

- **Overloading needs a parallel item pipeline** because the ordinary item query key cannot identify one candidate inside an overload group. This also makes generic overloads structurally unsupported and can produce a rootless `ItemFailed` diagnostic for a generic/non-generic overload pair. Fixing it means changing resolver/query identity rather than adding another special case.

- **Module paths and item paths are the same untyped `Vec<Ident>`**, so nothing in the type system prevents confusing a module identity with `module + item`. A distinct/interned path model would be cross-crate and is intentionally deferred.

- **`ModuleResolver` is still a broad semantic service facade.** The dependency direction is sound (analyzer asks, driver owns lifetime), but one trait currently spans imports, item lookup, generic signatures, overloads, specs, conformances, compile-time bodies/values, macro-origin metadata, and synthetic IDs. A narrower capability/query model is a deliberate future architecture change, not something to simulate with more ad-hoc helpers.

- **`reveal` activation is centralized but not structurally enforced by place resolution.** `RevealState` and the shared operand helpers remove the old raw-frame duplication and nested-reveal warning bug, but a future syntactic position can still forget to enter the helper before resolving a revealed place.

- **`Driver::compile(&mut self)` looks reusable even though its semantic/query state is one-shot.** The CLI only compiles once per driver today, but a second call can retain failed queries, registrations, diagnostics, and materialized bodies. The future API should either consume the driver or split reusable workspace/module state from a fresh `CompilationSession`.

- **Nominal semantic types use shared `Rc<RefCell<Resolved*Type>>` cells.** They solve recursive declaration identity cleanly today, but make phase completion an interior-mutability convention. Stable interned type IDs backed by an arena/query store would be a better prerequisite for incremental or parallel semantic analysis.

## Design debt worth watching

- **The contextual-keyword set grows with every feature, with no promotion
  policy.** Eighteen words are now position-dependent keywords
  (`parser::contextual`). Each one is a place where a lookahead can commit
  too early and silently stop the word being usable as a name — three had
  already done exactly that. The registry plus its generated test make the
  set visible and guarded, but there is no stated rule for when a word
  should graduate to a real reserved keyword instead.
  [parsing-and-hir.md](../architecture/parsing-and-hir.md)
- **Omega's calling convention is not the platform C ABI.** The largest
  piece of deliberate debt in the compiler, and it was *deliberately
  preserved unchanged* when the LLVM backend landed — mirrored rather than
  fixed, so separately compiled packages keep agreeing with each other. Two
  facts, both in `omega-codegen/src/abi.rs`:

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
  every target, because every separately compiled `omgc` invocation reads
  the same `AbiSignature` and therefore agrees exactly. Only the **C
  boundary** is wrong, and only for aggregates passed or returned **by
  value**. Scalars and pointers — all Omega's C interop uses today — are
  correct.

  To keep it that way rather than waiting for someone to discover it,
  an unsupported composite/by-value shape under a **non-Omega convention**
  (`c`, `sysv64`, ...) is a **hard error**
  (`AnalysisErrorKind::UnsupportedConventionByValue`) pointing back at this
  entry. `foreign` linkage by itself is not the trigger: a bare `foreign`
  function uses `CallingConvention::Omega` and the ordinary `AbiSignature`,
  so it accepts Omega composites by value like any other Omega call.
  `analysis::abi` owns the classifier and is applied at foreign
  declarations/definitions, function-typed foreign bindings, and indirect
  calls through a non-Omega function type. One classifier to replace once a
  real per-target ABI classifier exists; until then it turns a silent
  miscompile into a compile error.

  Fixing it properly means per-target, per-convention ABI classification in
  `abi.rs` (eightbyte classification for SysV, AAPCS for aarch64, ...). The
  translation point is now settled as `foreign(cc)` (see
  [`foreign-function-interface.md`](../language/foreign-function-interface.md));
  what remains open is only the aggregate classifier itself, not where the
  boundary is spelled.

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
     byte-offset read path therefore disagree about where `inner` is. This
     is a layout-model bug, not a codegen one.
  2. `MirPlace::align` is derived from `type_alignment`, and codegen turns
     it into a real `align` on every load and store. `llvm::place::offset_align`
     weakens the claim by the access's own byte offset, so nothing is
     over-claimed *relative to the place's base* — but the base itself can
     still be over-claimed when reached through a pointer (`p: *Inner`
     deref claims 16), because of the propagation gap above. This can be
     miscompiled at `-O2`/`-O3`, where LLVM actually acts on the
     over-claimed alignment.

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
  [parsing-and-hir.md](../architecture/parsing-and-hir.md)

- **Type-level capture remains possible in macro-generated declarations.**
  Generic parameters and `Self` intentionally ignore macro origin, because
  they are substitution-bound rather than lexical bindings. A generated type
  parameter can therefore still capture a same-named type from a substituted
  argument. There is no in-tree instance; partitioning these bindings would
  break the `Self` uses in the primitive conformance macros.
  [macros.md](../language/macros.md)

- **Macro-authored unused locals are not linted.** Expansion spans are anchored
  at the invocation and carry no source-file identity, so reporting the lint
  would misleadingly blame the caller. Locals introduced by a macro are
  intentionally excluded from `unused variable`; caller-origin arguments are
  still use-tracked normally.
  [macros.md](../language/macros.md)

## Control flow

- **`&&`/`||` reject a `never`-typed operand, but the `if` form they desugar
  to accepts one.** `flag && exit(1)` fails with `'&&' requires 'bool'
  operands, found 'never'`, while the equivalent
  `if flag { exit(1) } else { false }` compiles — so the operator is
  strictly narrower than the desugaring it produces. This follows
  `analyze_if`'s existing rule for a condition rather than being new, and
  diverging-in-one-branch is rare in practice, but it is an inconsistency
  between two spellings the docs present as equivalent.
  [control-flow.md](../language/control-flow-and-operators.md)


- **`bool` now has two spellings for each connective, and both are
  supported.** `a & b` and `a && b` differ only in whether `b` is evaluated;
  same for `|` and `||`. This is what C, C++ and Rust all do and what
  programmers expect, but it is still two mechanisms for one concept — the
  cleaner endpoint would be `&&`/`||`/`!` on `bool` and `&`/`|`/`^`/`~`
  reserved for integers. That is a breaking change to any `core`/`std` code
  using `&`/`|` on `bool`, so it was **not** taken unilaterally.
  **Decision needed:** keep both, or drop `&`/`|`/`^` on `bool`.
  [control-flow.md](../language/control-flow-and-operators.md)
- **Chained comparison is permanently a syntax error.** `a < b < c` now
  reports `comparison operators are non-associative` (it previously
  surfaced as a confusing `expected ';'`), matching Rust. Python chains it
  instead. **Decision needed:** is rejection the permanent answer, or should
  chaining eventually mean the conjunction?
  [control-flow.md](../language/control-flow-and-operators.md)
- **`&&` took a spelling that already meant something.** Adding the `&&`
  token silently changed the meaning of `a&&b` written without spaces: it
  used to lex as `&` `&` and mean "bitwise-and `a` with the address of `b`"
  — a program that compiles (an integer and a pointer both coerce for `&`,
  see [primitives](../language/types-and-primitives.md)) — and now parses as the logical
  connective and fails type checking. `a & &b` with the space is unaffected,
  and `||` has no such collision because `|` is infix-only. This was
  accepted rather than designed: the same trade C and C++ make. **Decision
  needed:** leave it (and say so in the docs), or require whitespace around
  binary `&` so the two readings can never be confused.
  [control-flow.md](../language/control-flow-and-operators.md)
- **`comp <` and `reveal <` are always the operator, never a comparison.**
  Both are contextual keywords, so `comp`/`reveal` are legal variable names,
  and both commit to the prefix-operator reading as soon as something that
  could be an operand follows. A leading `<` can begin a cast
  (`comp <usize>N`, which has always been valid), so it must count as an
  operand — which means a *variable* named `comp` can never be the left side
  of a `<` comparison. No single-token lookahead separates the two readings.
  **Decision needed:** accept the asymmetry, promote these two words to real
  keywords, or give casts a spelling that does not start with `<`.
  [parsing-and-hir.md](../architecture/parsing-and-hir.md)

- **A bare `return;` is a parse error**, so a `void` function cannot return
  early at all — `expected an expression, found ';'`. Every early exit in a
  `void` body has to be restructured around a sentinel flag, which
  old fixed-buffer I/O helpers had to do; the current `std::io::read_line`
  loops with a sentinel flag instead.
  [control-flow.md](../language/control-flow-and-operators.md)
