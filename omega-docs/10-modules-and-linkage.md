# Modules, resolution & linkage

## The `omgc` CLI

```
omgc <entry-file> -o <output> [--name=<name>] [--extern=[<name>:]<file>]... \
     [-O<0-3>] [--target=<triplet>] [--emit=<obj|ir|asm>] [-v]
```

`-o` is **required** — no default output path. `--extern` may be repeated;
each one points **directly at another project's own entry file** (not a
directory), e.g. `--extern=mathlib:examples/extern_lib/mathlib.omg`. A real
build, from this repo's own `justfile`:

```
omgc omega-core/core/core.omg --name=core -o target/core.o
omgc examples/extern_lib/mathlib.omg -o target/mathlib.o
omgc examples/dev/main.omg \
     --extern=mathlib:examples/extern_lib/mathlib.omg \
     --extern=core:omega-core/core/core.omg \
     -o target/main.o
cc target/main.o target/mathlib.o target/core.o -o example
```

Each `--extern`/entry file is compiled by its **own separate** `omgc`
process and linked afterward with a plain linker (`cc`) — there is no
whole-program single-invocation build. This works because module identity,
symbol mangling, and linkage are all deterministic, pure functions of
source text (see below) — two independent processes agree on symbol names
without ever communicating.

## Module identity

A module's identity (used for both name resolution *and* symbol mangling)
defaults to its file's stem, overridable: `--name=<name>` for the entry,
`--extern=<name>:<file>` for an extern. There is **no separate alias
concept** — whatever name an extern ends up with (inferred or explicit) is
simultaneously what `import extern::<name>;` selects it by *and* its real
mangled-symbol identity. Two different files claiming the same declared
name is a hard `DuplicateModuleIdentity` compile error (checked before any
parsing happens) — this used to silently misroute imports via a plain
`HashMap::insert` collision; the user flagged it directly as "a bomb
waiting to blow up," not a nice-to-have.

A directory-shaped package's own content lives **nested one level inside**
its own directory (`omega-core/core/core.omg`, not
`omega-core/core.omg`) — `fs_resolve` treats `dir/name.omg` and `dir/name/`
as competing siblings, so a package's real content can't sit beside its own
directory. `--extern`/`--name` point directly at that real, nested file;
`omgc` auto-detects the `dir/dir.omg` nesting convention (parent directory
name equals the file's own stem) and searches from the grandparent
directory instead — no sentinel/nonexistent placeholder path is needed
anywhere in the project (an earlier revision of the toolchain required
one; the user pushed back on it directly as bad design, and it was
removed).

## Imports

```
import sibling;                # relative to the importing module's own directory
import root::simplemodule;       # escapes to the project's own root
import extern::mathlib;             # into a registered --extern project
import hidden extern::lib;             # bypasses visibility (see visibility.md)
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
Root cause: several whole-program sweeps in `omega-driver` iterated a
`HashMap` directly to decide processing/emission order — a module's own
item sweep, the overloaded-function sweep (twice), the generic-instantiation
merge, the unused-import sweep, and the dead-code sweep (`sweep_dead_code`,
over `struct_cells`/`union_cells`/`enum_cells`) — plus one spot in
`omega-analyzer`'s own `warn_unused_bindings`, iterating a scope's
`declared_variables`. `HashMap`'s iteration order is per-process-random
(SipHash-seeded), so which order these produced their side effects (minting
a globally-sequential synthetic `HirId` per spec-default-method
instantiation, or simply the order items/warnings get pushed onto a `Vec`)
varied build-to-build for identical source — harmless within any one
compilation, but real object files (and diagnostic output order) differed
across repeated builds. Fixed by sorting each of these before iterating: by
declaration index for items, by each `decl_id`/span for warnings/imports,
and by a `Display`-derived key for generic instantiations (`ResolvedType`
has no `Ord` of its own, but a stable string key is enough here — this is
about determinism, not meaningful ordering). Verified empirically, not just
reasoned about: `just clean && just run-exec`, run repeatedly against
unmodified source, produced byte-different diagnostic output (confirming
the bug was real and reproducible) before this fix and byte-identical
output (diagnostics, order, and program output alike, module compile
timings/`cargo`'s own parallel build log excepted) across 7 consecutive
fresh runs after it.

## Caveats

- **Cross-compilation code *generation* is not shared, only the final
  *link*.** Every `omgc` invocation that references a generic
  instantiation still fully regenerates it locally; weak linkage only lets
  the *linker* discard duplicates afterward. There is no cross-process
  build cache.
- Codegen has no real C-ABI aggregate-passing convention — see
  [primitives](01-primitives.md)'s caveat.
