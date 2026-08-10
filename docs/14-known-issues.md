# Known issues tracker

A single consolidated list of every confirmed, currently-unfixed gap
described in these docs, for tracking at a glance. Each entry links to its
full writeup. Update this file whenever a gap here is fixed (move it to a
"Fixed" note in the relevant topic file, don't just delete the line) or a
new one is found.

## Codegen

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

## Types

- **`*str` is not actually guaranteed valid UTF-8** — casting between
  `*str` and `*[?]u8`/`*[?]i8` is unsound in both directions, no validation.
  Deliberately deferred pending a `core`-provided validating conversion.
  [strings-casting-and-slices.md](11-strings-casting-and-slices.md)
- **`char`/pointer arithmetic and `bool` logical-not are fixed; casting an
  arbitrary integer into `char`/`bool` and the `!` operator are still not
  implemented.** `char`/pointers now get arithmetic/bitwise ops (and
  casting out to any numeric type) by coercing to `u32`/`usize` first,
  never back implicitly; `bool` now gets native `== != & | ^`. What's
  still missing: casting an arbitrary integer *into* `char` (only `u8` is
  guaranteed to produce a valid codepoint, so only that direction is
  allowed — a real validating path, e.g. a fallible `char::from_u32`-style
  constructor, is deliberate future work, not solved narrowly here), and
  a `!` (logical-not) operator for `bool` (a real, if small, language
  feature — a new parser token plus a new `Expression`/`HirExpr`/
  `CheckedExpr`/`MirExpr` variant — left for a dedicated follow-up rather
  than folded into an otherwise analyzer-only change).
  [primitives.md](01-primitives.md), [control-flow.md](03-control-flow.md)
- **`core::fmt`'s float output is fixed-precision, not round-trip** — six
  fractional digits, with a scientific fallback below `1e-6` and at or above
  `1e19` whose normalization loop (repeated multiply/divide by ten) is itself
  lossy. `nan`/`inf`/`-inf` are exact. A shortest-round-trip formatter
  (Ryu/Grisu-class) is deliberate future work, not a narrow fix here.
  [console-io.md](24-console-io.md)

## Specs

- **Coercion into `spec *T` isn't wired into every expression position**
  (struct-literal fields, array-literal elements, bare tail-return without
  `return` are missing). [specs.md](08-specs.md)
- **No `is_variadic` support on spec functions.** [specs.md](08-specs.md)
- **`spec T` (static-dispatch) return-type inference isn't supported on
  struct/enum/union methods or overloaded free functions** — only a plain,
  non-overloaded top-level function can infer its return type from its own
  body; a method needing this hits `SpecStaticNotAllowedHere` instead.
  [specs.md](08-specs.md)
- **A type implementing `ToIterator<T>` more than once, at different `T`,
  has no disambiguation mechanism** — the explicit-cast escape hatch
  (`<spec *ToIterator<u64>>expr`) stopped working once `ToIterator<T>`
  became not-object-safe. [for-in-loops.md](18-for-in-loops.md)

## Gaps and glue

- **No default-bodied `@gap` function** — every gap function must
  currently be a bare requirement; a body is rejected outright
  (`GapFunctionBodyNotYetSupported`). [gaps-and-glue.md](21-gaps-and-glue.md)
- **No generic gap convenience overloads** (e.g. `alloc<T>()` alongside
  `alloc(size: usize)`) — blocked on the pre-existing overload-pipeline
  bug just above, not specific to gaps.
  [gaps-and-glue.md](21-gaps-and-glue.md)
- **No "override" or test-only glue concept** — a second `@glue` for the
  same gap is always a hard error project-wide, with no way to shadow one
  intentionally. [gaps-and-glue.md](21-gaps-and-glue.md)
- **One `@glue` marker cannot implement two gaps that share a function
  name.** Glue lowering exports one symbol per marker method, so a marker
  implementing both `StandardOutput` and `StandardError` — whose required
  function is `write` in both — can only carry one `write` body and silently
  loses a gap symbol. `plat`'s `libc` platform works around it with one
  marker per stream (`LibcStandardOutput`/`LibcStandardError`/
  `LibcStandardInput`). This constrains gap naming project-wide: two gaps a
  single platform is likely to implement together must not share a function
  name. [gaps-and-glue.md](21-gaps-and-glue.md),
  [platform-glue.md](22-platform-glue.md)

## Visibility

- **No re-export / `pub use`-equivalent.** Matches the language having no
  re-export concept at all today. [visibility.md](07-visibility.md)

## Modules

- **A `for`-attached extension method's own internal calls to a sibling
  extension method on the same type lose visibility when the type is
  instantiated from a consuming `--extern` package** — e.g.
  `core::slices`'s `SliceImpl<T>`'s `first`/`last` calling `self.get(...)`
  internally reports `get` as "not visible here" once `*[?]T` is actually
  used (not merely declared) from outside `core`. Reproduces identically
  on the baseline commit with an explicit `import core;` and no ambient
  resolution involved at all, so it's unrelated to the ambient-prelude
  work — a latent bug in how a `for`-block's own accessor context is
  threaded through when its methods are instantiated across a package
  boundary. [modules-and-linkage.md](10-modules-and-linkage.md),
  [specs.md](08-specs.md)

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
- **A macro body's nested invocations resolve at the call site, not the
  definition site**, because expansion is textual splicing into the caller's
  environment. A macro exported from one package therefore cannot call a
  helper macro that the caller cannot also see, and the resulting
  `UnknownMacro` names the *inner* macro with no indication that it came from
  an expansion. The fix is a per-definition home environment carried
  alongside each `MacroDefinitionStmt`. [macros.md](12-macros.md)
- **Importing a macro leaves a spurious `unused import` warning.** Macro
  names are resolved and consumed by the pre-pass in `omega-driver`'s
  `Driver::macro_env`, entirely before HIR exists, so the ordinary
  import-usage tracking never observes the use and reports the import as
  dead. Every cross-package macro import warns today.
  [macros.md](12-macros.md), [visibility.md](07-visibility.md)
- **A macro-generated `import` contributes no macros.** Item-position
  expansion can emit a real `Item::Import`, but macro resolution already ran
  against the file's hand-written imports by then. Deliberate and documented,
  not a bug to fix locally — the alternative is expanding to a fixpoint.
  [macros.md](12-macros.md)
- **A statement-position expansion's locals capture caller expressions of the
  same name.** The expansion is spliced into the caller's block, so an
  argument naming a caller variable binds to a macro-introduced local that
  shadows it. Wrapping the body in `{ }` prevents the local from leaking out
  but not from capturing in — that block is the scope the argument lands in.
  Hit for real: `core::io`'s print macros originally named their writer `out`,
  which silently shadowed the `*str out` in `examples/dev/main.omg` and made
  `println$(..., out)` fail with `no field 'fmt' on 'Writer'`. Worked around
  with an `omega_print_` prefix, which is a convention, not a guarantee. A
  real fix is gensym or a hygiene scope. [macros.md](12-macros.md)

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
- **Two independent pending-spec-method queues** that differ only in
  whether the owner has a declared item to key on.
- **`core` is hardcoded as the only place a `for` block may be declared**,
  so no third-party package can ship extension methods.
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

- **Taking a slice of a local array doesn't count as a use of that array**,
  so `mut b : [8]u8; s := &mut b[0..]; s[0] = 1u8;` warns `unused variable`
  for *both* `b` and `s`. Writing through a slice isn't tracked as a read of
  its base either. Previously obscure; now unavoidable, because
  `core::io`'s `println$`/`print$`/`eprint$`/`eprintln$` all declare a
  `mut buf : [256]u8;` in their expansion, so **every print statement emits a
  spurious `unused variable 'buf'`** — a hello-world program warns. Compounded
  by the composite-span issue below, the label points past the end of the
  file. The fix is in the analyzer's use-tracking (a slice/`&mut` of a place
  is a use of that place), not in `core::io`.
  [console-io.md](24-console-io.md)

## Control flow

- **A bare `return;` is a parse error**, so a `void` function cannot return
  early at all — `expected an expression, found ';'`. Every early exit in a
  `void` body has to be restructured around a sentinel flag, which
  `core::io::Writer::write_bytes` and `std::io::read_line` both had to do.
  [control-flow.md](03-control-flow.md)
