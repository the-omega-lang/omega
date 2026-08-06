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

## Parser

- **A bare, block-shaped `if` statement (no `else`) immediately followed
  by a new statement starting with `*`/`-`/`&` is misparsed** as a binary
  operator continuing the `if`'s own value into the next statement. An
  explicit trailing `;` after the `if` block works around it today.
  [control-flow.md](03-control-flow.md), [standard-library.md](23-standard-library.md)
- **A cast target ending in a nested generic's own closing `>`,
  immediately followed by the cast's own closing `>`, lexes as one `>>`
  token instead of two** (e.g. `<*mut Node<T>>0`). An explicit space
  before the cast's closing bracket works around it today.
  [strings-casting-and-slices.md](11-strings-casting-and-slices.md),
  [standard-library.md](23-standard-library.md)

## Types

- **`*str` is not actually guaranteed valid UTF-8** — casting between
  `*str` and `*[u8]`/`*[i8]` is unsound in both directions, no validation.
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

## Visibility

- **No re-export / `pub use`-equivalent.** Matches the language having no
  re-export concept at all today. [visibility.md](07-visibility.md)

## Modules

- **A `for`-attached extension method's own internal calls to a sibling
  extension method on the same type lose visibility when the type is
  instantiated from a consuming `--extern` package** — e.g.
  `core::slices`'s `SliceImpl<T>`'s `first`/`last` calling `self.get(...)`
  internally reports `get` as "not visible here" once `[T]` is actually
  used (not merely declared) from outside `core`. Reproduces identically
  on the baseline commit with an explicit `import core;` and no ambient
  resolution involved at all, so it's unrelated to the ambient-prelude
  work — a latent bug in how a `for`-block's own accessor context is
  threaded through when its methods are instantiated across a package
  boundary. [modules-and-linkage.md](10-modules-and-linkage.md),
  [specs.md](08-specs.md)

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

Language-level, not internal:

- **A value `match`'s arms must partition the domain exactly** — arms may
  not overlap, so a trailing `... => x` catch-all is never legal.
  [design-review.md](17-design-review.md#compiler-architecture)

## Design debt worth watching

- Every new `Expression`/`HirExpr`/`CheckedExpr` variant needs updates
  across up to five separate exhaustive matches spread over multiple
  crates (macro expansion, prelude re-exports, HIR lowering, defer-id
  collection, codegen emission) — the compiler catches every miss as a
  hard exhaustiveness error, so nothing is silently skipped, but budget
  for it when adding new expression forms.
