# Gaps and glue (`@gap`, `@glue`)

Some functionality `core`/`std` needs has no single, portable
implementation — a heap allocator is the motivating case: the actual
`alloc`/`free`/`realloc` behavior is genuinely platform-specific, and
exactly one concrete answer has to come from *somewhere downstream* of the
library declaring the need for it, usually the final application. `@gap`/
`@glue` is that mechanism: a `spec` declares a capability and is tagged
`@gap`; a `marker`, tagged `@glue`, supplies the one project-wide
implementation; any code anywhere calls the gap's own qualified name
directly, as if it were a marker's static function.

```
# in core::glue
@gap
exposed spec GlobalAllocator {
    alloc(size: usize) => *u8;
    free(ptr: *u8) => void;
    realloc(ptr: *u8, size: usize) => *u8;
}

# in the final application, anywhere
@glue
exposed marker Allocator : core::glue::GlobalAllocator {
    exposed alloc(size: usize) => *u8 { /* ... */ }
    exposed free(ptr: *u8) => void { /* ... */ }
    exposed realloc(ptr: *u8, size: usize) => *u8 { /* ... */ }
}

# anywhere that needs to allocate
my_buffer := core::glue::GlobalAllocator::alloc(64);
```

## Design summary

Both annotations are bare (no arguments) — the gap/glue relationship is
never named separately; it's the ordinary `implements` clause
(`marker Allocator : core::glue::GlobalAllocator`) that says which gap a
glue satisfies, exactly the same syntax any spec conformance already uses.

- **`@gap` binds to a `spec` only.** Every one of its functions (there are
  no default-bodied ones yet — see "Known limitations" below) must be
  self-less (no `self` at all, `AnalysisErrorKind::GapFunctionMustBeStatic`)
  — a gap function is always a static, symbol-bound call; there's no
  instance to hang a `self` off. Rejected on a `for`-attached spec
  (`GapOnForSpec` — no name of its own to glue against), a spec alias
  (`GapOnSpecAlias` — no function list to make into requirements), and a
  generic spec (`GapMustNotBeGeneric` — a gap's own expected linker symbol
  is computed once, for the whole spec; a generic instantiation has no
  separate one).
- **`@glue` binds to a `marker` only** (`GlueOnNonMarker`), never a
  generic one (`GlueMustNotBeGeneric`, same reasoning as
  `GapMustNotBeGeneric`). Every spec named in its `implements` clause must
  itself be `@gap` (`GlueOnNonGapSpec`) — a glue may implement *multiple*
  gaps at once, each validated independently, but never an ordinary spec
  alongside them (there's little practical use for a glue also
  implementing an unrelated ordinary spec, and allowing it would blur what
  the glue's actual job is). Ordinary spec conformance — every non-default
  function present, matching signature — is unchanged, still
  `Analyzer::resolve_implements_clause`'s job; `@glue` adds nothing new
  there.
- **Exactly one glue per gap, project-wide**, checked once, at the end of
  compilation (`Driver::sweep_gaps`): two or more markers implementing the
  same gap is `AnalysisErrorKind::MultipleGluesForGap`, naming every
  conflicting implementor. "Project-wide" is genuinely whole-program, not
  just "whatever this compilation happened to reference" — every
  registered `--extern`'s own struct/spec surface is eagerly resolved
  regardless of import (`Driver::collect_extern_signatures`, see
  [modules & linkage](10-modules-and-linkage.md)'s "Eager local
  discovery"), so two different externs each shipping an unimported
  `@glue` for the same gap are still compared and still caught here,
  rather than silently reaching the linker as a raw duplicate-symbol
  error (both would already be compiled into their own object files
  unconditionally — see "Across an `--extern` boundary" below).
- **An unglued gap is a warning, never an error**
  (`AnalysisWarningKind::UnfilledGap`) — deliberately: proving a gap is
  never actually *called* would need whole-program reachability analysis
  through indirect calls, which this design specifically avoids (see
  "Why the linker, not the frontend, catches a missing glue" below). The
  warning names every missing function so a later bare linker "undefined
  reference" (naming the *mangled* symbol, not these plain names) is
  still traceable back to it.

## How a gap and its glue end up at the same symbol

A gap function's expected linker symbol is computed exactly as if the gap
spec were an ordinary marker, in its own declaring module:
`mangle::method_symbol(gap_module_path, gap_name, &[], fn_name, fn_type)`
(`omega-codegen/src/mangle.rs`'s `glued_symbol`) — the same mangling
scheme every struct/enum method already uses, no new symbol format. This
reuses `@mangling(force = "...")`'s own machinery
(`omega_analyzer::annotations::ManglingMode`, see
[09-annotations.md](09-annotations.md)): a fourth mode,
`ManglingMode::Glued { spec_module_path, spec_name, function_name }`,
carries just enough for codegen to derive that identical string lazily,
rather than precomputing it in the analyzer (which has no dependency on
the mangling algorithm at all, and shouldn't gain one just for this).

- **The gap side**: every required function becomes, at codegen time, an
  ordinary extern declaration (`Linkage::Import`) under that computed
  symbol — indistinguishable from a hand-written top-level `extern`,
  except the compiler synthesizes it (`Driver::synthesize_gap_items`,
  reusing the exact `CheckedItem::ExternDeclaration` shape) rather than
  the user writing one. A spec declares no code of its own otherwise (see
  `check_item_body`'s own `HirItem::Spec` arm) — this is the one place a
  gap's own functions turn into anything codegen can see.
- **The glue side**: when a `@glue` marker's `implements` clause is
  resolved, every one of its own methods matching one of the gap's
  function names (by name — the signature match was already verified by
  ordinary spec conformance) has its `ManglingMode` forced to `Glued`,
  overriding whatever it would otherwise have picked
  (`Analyzer::collect_methods`). Only the marker's *own* methods are
  eligible — a method inherited from a spec default is compiled once, in
  the gap's own declaring module, never per-glue (moot today, since
  default-bodied gap functions aren't supported yet either — see below).
- **Within one compilation**, `cranelift_module`'s own `Linkage::merge`
  (already relied on by `Codegen::declare_function_def`) resolves an
  `Import` declared before its matching `Export` is defined, so the
  wiring happens entirely inside the compiler's own symbol table — no
  external linker round-trip needed when a gap and its glue are compiled
  together.
- **Across an `--extern` boundary** (the realistic shape —
  `core::glue::GlobalAllocator` is meant to be referenced this way, glued
  by whichever application actually needs it, not compiled inline with
  its own glue) — an extern-visible gap's required functions are also
  surfaced through `Driver::collect_extern_functions`, the exact same
  mechanism that already lets an extern struct's methods or a free
  function be called across a separate `omgc` invocation. The consuming
  compilation declares its own `Import`, the glue's own (separately
  compiled) `Export` satisfies it, and the real system linker resolves
  the two `.o` files' matching symbols at final link time. Confirmed
  working end to end: a `@glue` marker in an application, implementing
  `core::glue::GlobalAllocator` via `--extern=core:...`, links and runs
  correctly — and this holds even when the `@glue` marker's own module is
  never `import`ed by the application at all, only registered via
  `--extern`, since its signature (and `@glue`/`implements` status) is now
  resolved eagerly rather than only on reference.
- **A gap with no glue at all** leaves its `Import` genuinely unresolved —
  if nothing ever calls it, nothing ever references the symbol, and the
  program links fine regardless (matching `UnfilledGap`'s own "this only
  matters if something actually calls this gap" note). If something does
  call it, the system linker reports an ordinary "undefined reference" to
  the mangled symbol at final link time.

## `GapSpec::function(...)` — calling a gap directly

A qualified path whose prefix resolves to a `@gap` spec resolves its
trailing segment directly against that spec's own function list
(`Analyzer::resolve_type_member`'s new `ResolvedType::Spec` arm,
`analysis/paths.rs`), exactly like `Struct::static_fn()` already does for
an ordinary struct's static method — sharing that same shared tail
(visibility check, "too many segments", the final `CheckedPlaceRoot`) via
a synthetic `ResolvedMethod` built from the gap's own already-resolved
signature. This is a plain, statically resolved direct call, never
dynamic dispatch: `core::glue::GlobalAllocator::alloc(...)` compiles to
exactly the same shape a marker's own static call would, just referencing
the (possibly not-yet-known) glue's eventual symbol.

A gap function's signature is resolved once, eagerly, right where the gap
spec itself is declared (`Analyzer::resolve_spec_functions`, which also
does the `@gap`/self-lessness validation) — unlike an ordinary spec
function, which stays an unresolved `RawSpecFunctionSig` until a concrete
implementor's `Self` is known. A gap has no `Self` to wait for at all
(every function is self-less by construction), so there's nothing gained
by deferring; `ResolvedSpecType::gap_functions` holds the fully resolved
`(name, GapFunction { decl_id, span, fn_type })` list, read by both call
resolution and codegen's synthesis step.

A *non*-gap spec used the same way (`SomeSpec::function(...)`) is
unaffected — still the ordinary `AnalysisErrorKind::StaticAccessOnNonStruct`
("only structs and enums have functions").

## Why the linker, not the frontend, catches a missing glue

An earlier design considered a stricter, compile-time-only check: reject
the whole program if any gap is unglued. That would need proving whether
the gap's symbol is genuinely *reachable* — and precise reachability
through function pointers/dynamic dispatch is exactly the indirect-call
problem this compiler has deliberately not solved. Deferring to the
ordinary linker sidesteps it entirely: the linker doesn't care whether a
reference to a symbol came from a direct call or an indirect one, it just
resolves whatever ended up in the object file. `UnfilledGap`'s warning
exists purely to make the resulting "undefined reference" traceable back
to its cause, not to replace the linker's own, already-correct judgment
of whether the symbol is actually needed.

## Why `@gap`/`@glue` are unrestricted to `core`

Unlike a `for`-attached spec (restricted to `core` — see
[08-specs.md](08-specs.md)/[17-design-review.md](17-design-review.md)),
`@gap` carries no such restriction, deliberately. A `for`-spec's
restriction exists for a coherence reason: it lets you retroactively
attach behavior to a primitive type you don't own, and allowing that from
anywhere risks two unrelated libraries fighting over the same type. A gap
declares a *new*, self-owned extension point under its own module path —
there's no foreign type being fought over, so the coherence concern
doesn't transfer. Any library can have a legitimate "exactly one
implementation, chosen by the final application" need (a logger's sink, an
RNG's entropy source, a clock source), not just `core`.

## Known limitations

- **Default-bodied gap functions aren't supported yet**
  (`AnalysisErrorKind::GapFunctionBodyNotYetSupported`) — every `@gap`
  function must currently be a bare requirement, with every `@glue`
  providing its own body. Supporting a default (compiled once, in the
  gap's own declaring module, callable without any glue at all) needs the
  same synthetic-`HirFunctionDef`-reconstruction machinery an ordinary
  spec default method already uses (`Analyzer::check_pending_spec_method`)
  — left for a dedicated follow-up rather than folded into this feature's
  first cut, whose only shipped gap (`core::glue::GlobalAllocator`) has no
  default-bodied functions at all.
- **No generic gap convenience overloads** (e.g. a hypothetical
  `alloc<T>()` alongside `alloc(size: usize)`) — blocked on a separate,
  already-documented bug: a non-generic and generic overload of the same
  name currently produces an opaque, rootless diagnostic rather than
  either working or cleanly rejecting (see
  [17-design-review.md](17-design-review.md#compiler-architecture),
  "Overloading is a second, parallel item pipeline"). Not specific to
  gaps at all, but it scopes what a gap's own function list can look like
  until that's fixed separately.
- **A gap can currently only be implemented once, ever, in the whole
  compilation** — there's no "override" or "test-only glue" concept; a
  second `@glue` for the same gap is always a hard error, even if one of
  the two would otherwise be unreachable. Accepted as the simple, safe
  default for now.

## `core::glue`

`runtime/core/core/glue.omg` is the one place `core`'s own gaps live —
currently just `GlobalAllocator`. Building `core` standalone (`just
build-core`) correctly warns that `GlobalAllocator` has no glue yet — true
and expected, since no application exists at that point to provide one.

`plat::libc::glue::LibcAllocator` (see [`plat`](22-platform-glue.md)) is
the reference implementation — a plain `--extern` package, not part of
`core` itself, backing `GlobalAllocator` with libc's own `malloc`/`free`/
`realloc`. Registering `--extern=plat:...` is enough for any consuming
build to pick it up, with no `import` of `plat` required anywhere.
