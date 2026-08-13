# Modules, resolution & linkage

## The `omgc` CLI

```
omgc <entry-dir> -o <output> [--name=<name>] [--extern=[<name>:]<dir>]... \
     [-O<0-3>] [--target=<triplet>] [--emit=<obj|ir|asm>] [-v]
```

`-o` is **required** — no default output path. Both the local project and
every `--extern` are given as a **root directory**, never a file — the
filesystem, recursively walked, is the source of truth for what a package
contains (see "Eager local discovery" below), so there is no file for the
CLI to point at in the first place. A real build, from this repo's own
`justfile`:

```
omgc runtime/core/ -o target/core.o
omgc examples/mathlib/ -o target/mathlib.o
omgc examples/dev/ \
     --extern=mathlib:examples/mathlib/ \
     --extern=core:runtime/core/ \
     -o target/main.o
cc -Wl,--gc-sections target/main.o target/mathlib.o target/core.o -o example
```

Each `--extern`/local root is compiled by its **own separate** `omgc`
process and linked afterward with a plain linker (`cc`) — there is no
whole-program single-invocation build. This works because module identity,
symbol mangling, and linkage are all deterministic, pure functions of
source text (see below) — two independent processes agree on symbol names
without ever communicating.

Omega places each generated function in a separate object-file section. The
final link should use `--gc-sections` (as every repository `just` recipe does)
so an unused function from an otherwise-linked package cannot retain its
unrelated dependencies. This is what lets a core-only or allocator-only
executable link only the glue its reachable functions actually require.

## Module identity and the root module

A module's identity (used for both name resolution *and* symbol mangling)
defaults to its root directory's own basename, overridable: `--name=<name>`
for the local project, `--extern=<name>:<dir>` for an extern. There is
**no separate alias concept** — whatever name an extern ends up with
(inferred or explicit) is simultaneously what `import extern::<name>;`
selects it by *and* its real mangled-symbol identity. Two different
directories claiming the same declared name is a hard
`DuplicateModuleIdentity` compile error, checked once, at construction,
before any parsing happens.

The root directory **is** the root module. It may have an own source file,
named for the directory's physical basename and placed directly in that
directory; everything else in the directory is a child module:

```
runtime/core/option.omg     # core::option
runtime/core/strings.omg    # core::strings
```

The same convention already applies to a nested directory-shaped module:
`foo/foo.omg` owns `foo`, and `foo/bar.omg` owns `foo::bar`. At the root,
`<root>/<basename>.omg` owns the declared root module even when `--name=`
renames that module. A root with no own file — `runtime/core/` and
`runtime/std/` are examples — is a namespace-only module and may still
contain children.

`main.omg` has no special meaning. A function named `main` in the root module
receives the bare C entry symbol; a `main` elsewhere is an ordinary mangled
function. There is no library/program mode: a package without root-module
`main` simply has no C entry point, and the linker decides how its object is
used.

### Known gap: a same-named subdirectory hides itself, silently

`discover_tree` registers the root module and then walks the root's other
entries with `skip = <basename>`, so `<root>/<basename>.omg` isn't
double-counted as both the root's own file and a child called `N::N`. `skip`
matches by *name*, not by kind, so it also swallows a **directory**
`<root>/<basename>/`. If that directory is where the package's sources
actually live, discovery finds nothing inside the root at all.

That is exactly the pre-root-module layout (`runtime/core/core/`,
`runtime/std/std/`), so it is the shape a package being migrated is most
likely to be in. The *consequence* is now reported rather than silent:
a package that discovers no modules is `CompileError::EmptyPackage`, naming
the root, the module file that was looked for, and — in its help — the move
that fixes an old-layout package. Before that guard it was worse than silent,
reaching `compile`'s generic-instantiation merge and failing its
"always includes at least the entry module" expectation as a compiler panic.

What is still missing is the *specific* diagnosis: nothing says "the directory
`<root>/<basename>/` exists and was skipped", so a package that has both a
root module file *and* a same-named subdirectory of sources still loses the
subdirectory quietly. Narrowing `skip` to files only would fix it, but that
changes the recursive half of discovery, and the both-present case
(`<root>/<basename>.omg` **and** `<root>/<basename>/`) is already a proper
`AmbiguousModule` — so the remaining hole is narrow, and worth its own change
rather than a widening of this one. The same skip-by-name behaviour exists one
level down (`X/X/` under a directory-shaped module `X`), where it predates the
root-module rule rather than being introduced by it.

Resolving it is a design decision, not a local fix. Either a same-named
*directory* becomes a collision with the module its parent already owns,
reported like the both-a-file-and-a-directory case — but
`ResolveError::AmbiguousModule`'s wording ("both a file and a directory claim
this name") is untrue when there is no file, so that needs a reworded or new
variant — or `skip` narrows to the file `<name>.omg` alone and
`<root>/<basename>/` resolves as the ordinary child `N::N`, which is what
"everything else in the root is a child" literally says but changes the
recursive half of discovery that the root-module change deliberately left
untouched. Independently of which is chosen, a package that discovers no
modules at all should be a reportable error rather than an empty object file.

## Eager local discovery

The local project's root is **recursively, eagerly walked in full** the moment
`omgc` starts (`fs_resolve::discover_tree`) — metadata only (no file is opened
at this point, just `read_dir`/`is_file`/`is_dir`). Discovery registers the
root module first, then walks its children beneath that root path. The result
is a complete inventory of every module the local project contains; looking
one up (`ModuleRoots::locate`) is a plain map lookup afterward, and an absent
entry is a real, checked fact — "does not exist" — not "wasn't asked about
yet".

**This inventory is also, directly, the local package's own build set**
(`Driver::local_module_paths`) — every module it finds is parsed and
compiled, whether or not anything imports it. The filesystem is the
source of truth for what a package *contains*, full stop, not just for
what a *path* resolves to: nothing needs to import a sibling module for
it to be part of the build, only to *reference* its contents (imports
remain required for that — see "Imports" below). A module with a genuine
parse or macro-expansion error is caught with its full diagnostic detail
regardless of whether anything references it, the same as any other
module — there is no "not imported yet, so not checked" exemption for the
local package. This is still distinct from eager *analysis*: nothing
about type-checking a specific *item*'s body changed (a generic template,
say, is still only instantiated on demand) — only which *modules* are
swept at all is no longer import-graph-driven for the local package.

**`core` gets this same eager treatment too, unconditionally, regardless
of whether it's the package actually being compiled or a registered
`--extern`** (`ModuleRoots::core_modules`) — the one deliberate exception
to "an `--extern` dependency never gets this treatment" above. If `core`
is registered as an `--extern`, its own directory is eagerly walked the
same way the local root is, right at `ModuleRoots` construction; if `core`
*is* the local package (`just build-core`) or happens to live nested
inside it, `core_modules()` just filters the local inventory already built.
Either way, the result feeds the local build set (when `core` is local),
primitive/compose registration, and ambient/prelude name resolution (see "`core` as an ambient
prelude" below). No other `--extern` gets *this* — see that section
for why `core` specifically earns the exception.

**Every `--extern`, not just `core`, now gets its own eager tree
*discovery*** (`ModuleRoots::extern_trees`, generalized from what used to
be `core`-only) — every module path it contains is known upfront, the same
way `core`'s always has been. This is narrower than what `core` gets
above: knowing a path exists is not the same as eagerly parsing or
resolving it. What every extern's tree discovery *does* feed, uniformly:
`Driver::collect_extern_signatures` eagerly resolves every struct's and
spec's own *signature* (never a body, never a free function/overload/
enum/union as its own eager entry point) in every registered extern,
`core` included, before the local package's own signatures are collected.

This exists for one reason: `gap`/`glue` tracking
([gaps and glue](21-gaps-and-glue.md)) needs to see every gap and every
glue in the whole compilation, not just whichever ones happened to be
referenced. Before this, an extern module nobody imported was invisible
to that check entirely — a real `glue` sitting in an unimported sibling
module produced a false "unfilled gap" warning, and two different externs
each shipping an unimported `glue` for the same gap were never compared
at all, silently deferring a genuine conflict to a raw linker "duplicate
symbol" error. Now every extern's struct/spec surface is resolved
regardless of reference, exactly like the local package's already is, so
the existing gap/glue sweep sees the whole picture without needing any
logic of its own to change.

This is a real, unconditional, uncached cost paid on every single
compile — there is no incremental build to cache it against yet (see
[known issues](14-known-issues.md)) — and it also means a broken,
wholly unrelated struct or spec anywhere in *any* registered extern can
now fail a build that never references it, the same way it already could
for `core`'s own primitive/compose declarations. Deliberately not a general "every
extern behaves exactly like the local package" change, though: unlike
`core` (see "`core` as an ambient prelude" below), an ordinary extern
still gets no ambient/prelude name resolution — only this narrowly-scoped
signature and unnamed-declaration sweep.

## Imports

```
import sibling;                # relative to the importing module's own directory
import root::simplemodule;       # escapes to the project's own root
import extern::mathlib;             # into a registered --extern project
import reveal extern::lib;             # bypasses visibility (see visibility.md)
```

Default resolution is **relative to the importing module's own directory**
(a real, deliberate semantic choice — not always-absolute-from-root).
`root::` and `extern::` are contextual keywords recognized only as an
import path's leading segment.

Resolution is **lazy and per-alias**, not per-module: each `module_path,
alias` pair is memoized independently, the same fine granularity ordinary
item resolution already had. This exists specifically because the
alternative — fetching a module's *entire* import list before any one item
in it is touched — produced a real, confirmed false-cycle bug: two modules
whose *unrelated* items happened to cross-import each other's module
deadlocked resolving each other's whole list, even though the specific
items referenced never actually formed a cycle. Ordinary cross-module
function mutual recursion works fine; a genuine by-value struct cycle
across modules is (mostly — see caveat below) still correctly rejected.

**Extern modules are scanned, not compiled.** An extern module's ordinary
items resolve lazily, exactly like a generic instantiation, only when a
local item actually references one — never body-checked. The one
exception, besides `core`'s ambient primitive treatment above, is every
struct's and spec's own *signature*, now eagerly resolved regardless of
reference too (see "Eager local discovery" above) — purely so `gap`/
`glue` tracking sees the whole picture; nothing about ordinary name
resolution changed, an unreferenced struct/spec's signature being resolved
doesn't make it importable or visible any differently than before. A
*generic* template defined in an extern module is the one true exception
to lazy *body* resolution: its concrete instantiations are fully
(re)compiled locally, since nothing else will ever produce that exact
instantiation's body (see [generics](06-generics.md)).

**Primitive and compose declarations never need an import merely to be
registered.** They are discovered from the same eager package inventory as
named signatures. Only `core` may declare `primitive` blocks; compose blocks
in any package are admitted by the target-or-spec-local orphan rule. Imports
still control which spec names can be written at a declaration or qualified
call site.

A compose's method bodies are compiled into the **composing** package's own
object file, with ordinary linkage — not into the consumer's, and not into
the package that declares the target type. The symbol nests the target under
the spec (`<target>::<Spec>::<method>`), so two packages may attach
same-named methods to one foreign type without colliding: `alpha` declaring
`Foo`, `beta` composing `Foo : SpecB`, and an application composing
`Foo : SpecC` produce `alpha::Foo::SpecB::m` in `beta.o` and
`alpha::Foo::SpecC::m` in the application's own object, with the application
carrying an ordinary undefined reference to the first. This replaces the old
`for`-attached model, where extension methods were emitted with weak linkage
into whichever translation unit happened to *use* them.

## `core` as an ambient prelude

`core` is not an ordinary `--extern` (or, when it's the package actually
being compiled, an ordinary local package) — every name it exposes is
available *everywhere else*, with no `import core;` and no `core::`
prefix required, as if it were silently, recursively imported into every
other module. Two independent mechanisms combine to make this true,
both keyed off the same `core_modules()` inventory:

- **`core::X::Y` is always a valid qualified path**, even with no
  `import core;` anywhere in the file (`Driver::resolve_import_alias`'s
  fallback: an unresolved alias named `core` resolves to the `core`
  module itself, provided `core` is registered at all). `core`'s own
  files are the one exception — they still need real imports among
  themselves, the same as any other module, so a file inside `core`
  referencing another part of `core` unqualified doesn't quietly become
  self-referential.
- **A bare, unqualified name** (`GlobalAllocator`, not
  `core::platform::GlobalAllocator`; `Option<T>`, not `core::option::Option<T>`)
  resolves against every *exposed* item across all of `core_modules()`
  (`ModuleResolver::ambient_core_candidates`), tried only *after*
  ordinary local/import resolution of that name already failed — so a
  local declaration, or an explicit import, always wins outright; ambient
  resolution can never shadow either one. `core`'s own files are excluded
  from this fallback for themselves, same reasoning as above.

This applies uniformly everywhere a bare name can appear — a value
expression, a type annotation, a generic bound, or a compose declaration —
not just the `for`-in loop's `Option`/`Iterator`/`ToIterator` protocol
this mechanism originally existed for. This is a deliberate, considered
reversal of an earlier, narrower design (a hardcoded 3-name table, with
its own doc comment explicitly disclaiming "not a general prelude"): once
every one of `core`'s own modules is eagerly known regardless of
local/extern status anyway (see above), keeping the ambient set to those
three specific names stopped being a meaningfully smaller commitment,
while giving up real, everyday convenience the language could otherwise
offer.

**Two `core` modules independently exposing the same bare name is a
real, permanent compile error going forward** — not a hypothetical:
`AmbiguousAmbientName` names every exposing module and suggests the
fully-qualified path as the always-available escape hatch. Overloaded
core functions are deliberately excluded from bare ambient resolution —
a qualified reference is unaffected, but calling one by its bare name
alone, relying on ambient resolution to find it, isn't supported.

**Only `core` gets any of this — deliberately, not as a first step
toward a general "any extern can opt into prelude status" mechanism.**
It's justified by `core`'s already-privileged status elsewhere (it's the
only package a `primitive` block may live in); an ordinary
third-party `--extern` dependency gets neither eager discovery nor
ambient bare-name resolution, and still needs an explicit `import` for
every name it wants visible, exactly as before.

## Symbol mangling (`omega-mangle`)

A standalone crate implementing a scheme adapted from Rust's RFC 2603
(`v0` mangling), prefixed `_omg_`: decodable, `[A-Za-z0-9_]`-only, with
byte-offset backref compression built in from the start (not toggleable).
Deliberate deviations from a literal RFC port, all because Omega either has
a feature Rust's mangling omits or lacks one it needs to encode:

- **The full signature (params + self + return type) is part of the
  symbol** — required, since Omega has function/method
  [overloading](06-generics.md) and Rust's scheme doesn't.
- No `<impl-path>` (methods are declared directly on their owner, never
  through a separate/possibly-multiple `impl` block), no lifetimes, no
  disambiguator-index (no closures, and macro expansion is a pure
  pre-lowering token splice with no gensym — two distinct declarations can
  never collide once the full signature is part of the symbol), no
  Punycode (identifiers are ASCII-only).

A generic instantiation's symbol is a **pure function of `(module_path,
name, type_args)`** — genuinely stable across separate `omgc` processes
compiling the same source, unlike the old ad hoc `path::name$$<counter>`
scheme it replaced (an arbitrary per-process counter, not reproducible
build-to-build).

A companion `omg-demangle` CLI exists for reading mangled symbols back.

## Linkage: weak symbols for cross-TU sharing

Two independent things fold identical content across separately-compiled
object files at final link time, both riding on `cranelift-object`'s
`Linkage::Preemptible` mapping to real ELF/Mach-O/COFF weak binding
(confirmed empirically via `nm`/`readelf`, not just by reading the
`// TODO: ... may be wrong` comment sitting above that mapping in
Cranelift's own source):

- **Anonymous rodata** (string literals, `b"..."` byte strings, `&[...]`
  compile-time slice data) — named `_omgdata_<hash-hex>` (a fast
  non-cryptographic hash — `rapidhash`, swapped in from an initial `XXH3`
  choice purely for speed — over the *logical* content, not the physical
  buffer: a pointer-shaped element like a nested string is hashed by its
  real bytes, not the zero-placeholder bytes the physical buffer holds
  before relocation, otherwise two different string constants of the same
  length could collide onto one symbol). Two unrelated files sharing
  identical string content now genuinely fold into one copy in the final
  `.rodata`, confirmed via a real two-file, no-`--extern`-relationship
  link.
- **Every generic instantiation** (function, method, spec vtable) — see
  [generics](06-generics.md) for the full mechanism and its empirical
  diamond-dependency verification.

Ordinary, non-generic, non-anonymous-data symbols stay `Export` (strong) —
a genuine duplicate-definition user error (e.g. two `@mangling(disabled)`
functions sharing a name) still hard-fails at link time exactly as before;
this was explicitly verified as a negative control.

## Fixed: struct cycle through a bare import alias, and build reproducibility

**A cross-module, mutually-by-value struct cycle reached through a bare
(unqualified) import alias used to silently compile instead of erroring.**
Root cause: `Context::resolve_type`'s `Type::Named` unqualified-alias
branch trusted `ImportTarget::Item`'s eagerly-resolved snapshot directly —
that snapshot was always produced with `indirect: true` (classifying "what
does this alias mean" never itself embeds anything inline), so a by-value
struct field's real `indirect: false` never reached the cycle check, no
matter which of the two mutually-referencing structs happened to still be
`InProgress` at the time. Fixed by giving `ImportTarget::Item` its absolute
path alongside the snapshot, so this one consumer re-resolves through
`ModuleResolver::resolve_item` with its own real `indirect` instead of
trusting the cached value — exactly mirroring how a module-qualified
reference (`mymodule::Foo`) already worked. Every other consumer of
`ImportTarget::Item` (calls, literal construction — never inline-embedded
either way) is unaffected. Verified via `git stash`-diffed before/after: the
identical mutual-cycle input silently compiled before, now correctly
rejects with `RecursiveTypeWithoutIndirection`; mutual cross-module
*function* recursion (the false-cycle bug this whole lazy-per-alias design
exists to avoid) and a legitimate pointer-indirected struct cycle both still
compile clean.

**Object files for byte-identical source used to differ, build-to-build.**
Root cause: several caches across `omega-driver`/`omega-analyzer` are
`HashMap`s that also get *iterated as a whole* somewhere -- a module's own
item sweep and the overloaded-function sweep (both over a module's index),
the generic-instantiation merge, the unused-import sweep, the dead-code
sweep (over the struct/union/enum cells), a scope's own declared-binding
walk (`Analyzer::warn_unused_bindings`, over `declared_variables`), the
`for`-attached-extension drain, and a scope's own type-name
typo-suggestion lookup (`Context::similar_type_name`, over
`defined_types`). `HashMap`'s iteration
order is per-process-random (SipHash-seeded), so whichever of these ran
first produced side effects (minting a globally-sequential synthetic
`HirId` per spec-default-method instantiation, picking a "did you mean"
candidate on an edit-distance tie, or simply the order items/warnings get
pushed onto a `Vec`) in an order that varied build-to-build for identical
source -- harmless within any one compilation, but real object files (and
diagnostic content/order) differed across repeated builds.

Fixed by converting every one of these fields from `HashMap` to
`IndexMap` (insertion-order-preserving, same O(1) lookup) rather than
sorting at each iteration site -- insertion order already *is* the
meaningful order in every case (declaration order for items/bindings/
imports, first-reference order for cells, first-resolution order for
extensions), so this closes the bug at the type level: a *future* iteration
site over one of these caches inherits determinism for free instead of
needing to remember to sort. Verified empirically at every step, not just
reasoned about: `just clean && just run-exec`, run repeatedly against
unmodified source, produced byte-different diagnostic output and
byte-different object files (`cmp`/`nm -p`-confirmed differing function
declaration order) before each fix, and byte-identical output across
15+ consecutive fresh runs after -- including for two sites (the pending
composition queue and `defined_types`) whose nondeterminism doesn't
manifest in this project's own current source at all (nothing here ties two declared names at the same edit-distance from a
typo) and needed a dedicated repro to prove real, not just theorized.

## Fixed: driver restructure, and two bugs it exposed

`omega-driver` was a single 2 800-line file whose `Driver` held 30 flat
fields, eleven of them separate maps keyed by module path (parsed HIR, module
ids, directory-shape flags, sources, parse failures, macro failures, item
indices, overload indices, import aliases, errors, warnings). It is now eight
focused modules, and the state is grouped by concern: where modules come from
on disk, what has been parsed and indexed, what has been resolved, what each
import means, what primitive/compose blocks were found, and where findings accumulate.
The item query itself is unchanged in behavior — same two phases, same
per-item granularity, same cycle guard — and every object file this project
builds is byte-for-byte identical before and after.

Two real bugs fell out of the restructure:

**Dead-code warnings were reported once per generic instantiation, not once
per declaration.** The sweep walked the struct/union/enum *cells*, and a
generic type has one cell per instantiation. A field of `Holder<T>` unused by
both `Holder<i32>` and `Holder<u8>` produced the same warning twice, at the
same span; worse, a field used by `Holder<i32>` but not `Holder<u8>` was
reported as unused even though the warning's own text claims it "is never read
anywhere `Holder` is used". Instantiations of one declaration are now judged
together: a field is unused only when *no* instantiation touched it, and it is
reported once. This is why `examples/dev/dev.omg` no longer warns about
`Optional`'s `None` variant and `value` field — `Optional<u32>::None` is
constructed on line 849, so the old warning was simply false.

**`is_item_visible` searched a cache instead of reading the declaration.** The
query behind `UnnecessaryReveal` did a linear scan over every resolved item
looking for any entry that happened to share a module and name, taking
whichever one the hash order surfaced first, and answered `false` (not
visible) whenever nothing had been resolved yet. Visibility is a property of
the *declaration* — identical for every instantiation, and knowable without
resolving anything — so it now reads the declaration directly. Same answers,
no scan, no dependence on what happened to be cached.

Also removed: `ResolveError::MacroExpansionFailed`, a variant nothing had
constructed since macro failures started being reported structurally as
`CompileError::MacroExpansion`.

## Fixed: two bugs found building `std` against `core` as a real `--extern` consumer

Both surfaced only once a *second* real package (`std`) started calling
into `core`'s primitive methods and instantiating `core`-external generic
structs — a combination nothing had exercised end-to-end before.

**Unqualified-path resolution silently treated any imported non-generic
item as if it were declared locally.** `Context::resolve_absolute_item_path`
only had a match arm for `ImportTarget::GenericItem`; an imported ordinary
(non-generic) item fell through to the "not imported, must be local"
branch, which happened to still resolve correctly for *same-package*
imports (the path it built was accidentally right) but broke for any
cross-package alias, e.g. `spec Hashable = Hash | Eq;` failing to resolve
`Eq` with "'Eq' is not a spec". Fixed by adding the missing
`ImportTarget::Item` arm, mirroring `resolve_named_type`'s own
already-correct handling of the same case.

**A generic instantiation whose *template* lives in an extern package was
silently dropped from codegen.** `Driver::compile`'s merge loop matched
each pending instantiation's module against `modules` — which only ever
contains the *local* package's own modules — so any generic struct
declared in `core` (or any extern package) and instantiated by a consumer
had its instantiation computed, then discarded, never emitted. Invisible
until something used a generic external struct with no local generic
struct also present to "absorb" the merge by coincidence; a bare
`List<i32>::new()` alone was enough to reproduce it as a codegen panic
(`... was declared as a function before this use`). Fixed by falling back
to the first local module when no exact match exists — safe because each
module is lowered independently with no cross-module state, so which
local module "hosts" an extern instantiation doesn't matter.

## Fixed: `discover_into` double-counted a directory-shaped module named the same as its own entry file

A directory-shaped module's own children live in a directory matching its
name (`X/X.omg`) — `discover_into` recorded that entry, then recursed
into `X/` to find its children, and that rescan found `X.omg` again,
indistinguishable from an ordinary sibling submodule. The result was both
`X` and a spurious `X::X` pointing at the identical file — silent whenever
`X` declared nothing, but a real duplicate-definition/
`AmbiguousAmbientName` error the moment it declared anything and this
project's own `--extern` aliasing (see below) needed exactly that shape
to work. Fixed by threading through which single name to skip on that one
recursive call — it's already known to be the entry file just recorded
one level up, never a fresh sibling.

## Aliasing a root's declared identity, independent of its on-disk name

`--name=<name>` (standalone) and `--extern=<name>:<dir>` (as a dependency)
let a root's declared identity differ from its directory's own basename. The
root file remains named after the physical directory: `runtime/plat/libc/`
contains `libc.omg`, yet compiles and imports as `plat`. The alias applies to
the root and everything discovered *beneath* it
(`fs_resolve::relabel_root`, applied to `ModuleRoots::local_tree`/
`extern_trees`), so a directory honestly named `libc`, with real,
multi-item content of its own, can present in full as a different
package, e.g. `plat` (see [`plat`](22-platform-glue.md)). For the local
package specifically, this applies when `--name=` is given explicitly; an
extern's declared name is likewise applied to its whole discovered tree.

Making lookup agree with this required one more change: `ModuleRoots::
locate` used to reconstruct a filesystem path directly from a path's
*declared* segments for every `--extern` reference, live, one path at a
time — which would search for a literal `plat.omg` on disk regardless of
what's really there. It now reads the already-discovered, already-
aliased `extern_trees` inventory instead, a plain map lookup with no live
filesystem access — simpler than the old live-lookup path (a direct
consequence of every extern's struct/spec surface already being eagerly
discovered, see "Eager local discovery" above) and a prerequisite
for aliasing to work at all beneath the root.

## Caveats

- **Cross-compilation code *generation* is not shared, only the final
  *link*.** Every `omgc` invocation that references a generic
  instantiation still fully regenerates it locally; weak linkage only lets
  the *linker* discard duplicates afterward. There is no cross-process
  build cache.
- Codegen has no real C-ABI aggregate-passing convention — see
  [primitives](01-primitives.md)'s caveat.

Macros participate in ordinary item imports. Exposed macros in `core` are
also available as the final ambient-prelude resolution fallback; imports bind
their bare invocation names, so qualified macro invocation syntax is not
needed.
