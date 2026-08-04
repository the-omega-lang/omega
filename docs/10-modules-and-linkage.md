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
omgc examples/extern_lib/ --name=mathlib -o target/mathlib.o
omgc examples/dev/ \
     --extern=mathlib:examples/extern_lib/ \
     --extern=core:runtime/core/ \
     -o target/main.o
cc target/main.o target/mathlib.o target/core.o -o example
```

Each `--extern`/local root is compiled by its **own separate** `omgc`
process and linked afterward with a plain linker (`cc`) — there is no
whole-program single-invocation build. This works because module identity,
symbol mangling, and linkage are all deterministic, pure functions of
source text (see below) — two independent processes agree on symbol names
without ever communicating.

## Module identity, and the entry module

A module's identity (used for both name resolution *and* symbol mangling)
defaults to its root directory's own basename, overridable: `--name=<name>`
for the local project, `--extern=<name>:<dir>` for an extern. There is
**no separate alias concept** — whatever name an extern ends up with
(inferred or explicit) is simultaneously what `import extern::<name>;`
selects it by *and* its real mangled-symbol identity. Two different
directories claiming the same declared name is a hard
`DuplicateModuleIdentity` compile error, checked once, at construction,
before any parsing happens.

Given the local root directory and its declared identity, `omgc` finds the
entry module itself, trying two conventions in order: `<name>.omg`/a
directory-shaped `<name>/<name>.omg` (the *library* convention — the same
one any nested directory-shaped module's own content already follows,
applied to the root itself; right when the directory's name and the
package's own identity already agree, e.g. `core`, whose own content lives
at `runtime/core/core/core.omg` — a directory-shaped module named `core`,
nested one level inside `runtime/core/`, which is why that's the root
`--extern=core:` points at, not `runtime/core/core/` itself), else the
fixed, purpose-specific `main.omg` (the *executable* convention — right
when the directory's name has nothing to do with the program, e.g.
`examples/dev/`, whose default identity is `dev` but whose entry is
`main.omg`). Mirrors Rust's own `lib.rs`/`main.rs` split without an
explicit `--lib`/`--bin` mode flag. Neither present is a real,
reportable error, not a silent empty build.

## Eager local discovery

The local project's own root is **recursively, eagerly walked in full**
the moment `omgc` starts (`fs_resolve::discover_tree`) — metadata only (no
file is ever opened at this point, just `read_dir`/`is_file`/`is_dir`), so
this stays cheap regardless of how large the package is. The result is a
complete inventory of every module the local project actually contains;
looking one up (`ModuleRoots::locate`) is a plain map lookup afterward, and
an absent entry is a real, checked fact — "does not exist" — not "wasn't
asked about yet". An `--extern` dependency never gets this treatment
either way; it stays resolved lazily, one path at a time, on demand —
eager discovery belongs only to whichever package is actually *being
compiled* in this invocation, never to one merely referenced.

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
inside it, `core_modules()` just filters the local inventory already
built. Either way, the result feeds three separate consumers uniformly,
with no local/extern branch anywhere downstream: the local build set
(when `core` is local), `for`-block extension discovery (see "Imports"
below), and ambient/prelude name resolution (see "`core` as an ambient
prelude" below). No other `--extern` gets any of this — see that section
for why `core` specifically earns the exception.

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
local item actually references one — never eagerly swept or body-checked.
A *generic* template defined in an extern module is the one exception: its
concrete instantiations are fully (re)compiled locally, since nothing else
will ever produce that exact instantiation's body (see
[generics](06-generics.md)).

**`for`-block extension discovery never needs an import at all**, in
either direction: a spec `for`-attached to some type (`spec SliceImpl<T>
for [T] { ... }`, see [for-in loops](18-for-in-loops.md) for the iteration
protocol case) is discovered straight from `core_modules()` — the same
eager inventory the "Eager local discovery" section above describes —
never from walking anyone's import list. Only `core` may declare a
`for`-block at all (`extensions::CORE_MODULE`); every one of its own
modules is always in scope for this regardless of whether the file that
declares the `for`-block, or the file that ends up calling the method it
attaches, imports anything.

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
  `core::glue::GlobalAllocator`; `Option<T>`, not `core::option::Option<T>`)
  resolves against every *exposed* item across all of `core_modules()`
  (`ModuleResolver::ambient_core_candidates`), tried only *after*
  ordinary local/import resolution of that name already failed — so a
  local declaration, or an explicit import, always wins outright; ambient
  resolution can never shadow either one. `core`'s own files are excluded
  from this fallback for themselves, same reasoning as above.

This applies uniformly everywhere a bare name can appear — a value
expression, a type annotation, a generic bound, an `implements` clause —
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
only package a `for`-block may live in at all, see above); an ordinary
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
extension queue and `defined_types`) whose nondeterminism doesn't
manifest in this project's own current source at all (nothing here
currently leaves a `for`-attached spec default pending across multiple
receivers, or ties two declared names at the same edit-distance from a
typo) and needed a dedicated repro to prove real, not just theorized.

## Fixed: driver restructure, and two bugs it exposed

`omega-driver` was a single 2 800-line file whose `Driver` held 30 flat
fields, eleven of them separate maps keyed by module path (parsed HIR, module
ids, directory-shape flags, sources, parse failures, macro failures, item
indices, overload indices, import aliases, errors, warnings). It is now eight
focused modules, and the state is grouped by concern: where modules come from
on disk, what has been parsed and indexed, what has been resolved, what each
import means, what `for` blocks were found, and where findings accumulate.
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
reported once. This is why `examples/dev/main.omg` no longer warns about
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

## Caveats

- **Cross-compilation code *generation* is not shared, only the final
  *link*.** Every `omgc` invocation that references a generic
  instantiation still fully regenerates it locally; weak linkage only lets
  the *linker* discard duplicates afterward. There is no cross-process
  build cache.
- Codegen has no real C-ABI aggregate-passing convention — see
  [primitives](01-primitives.md)'s caveat.
