# The root directory *is* the package module

## Task Description

- **What is being asked:** Remove the redundant nesting in a package's on-disk layout.

  ```
  # before                        # after
  runtime/                        runtime/
    core/                           core/
      core/                           core.omg
        core.omg                      option.omg
        option.omg                    io.omg
  ```

  Three changes, one rule:
  1. **A package's root directory is its root module.** Its own file is
     `<dirname>.omg`, sitting directly in the root; everything else in the root is a child.
  2. **`main.omg` stops being a special filename.** A package is not declared to be a
     library or a program at compile time.
  3. **`main` in the root module** is the C entry point.

- **Purpose:** `runtime/core/core/core.omg` repeats `core` three times, and the repetition is
  *forced*, not stylistic. Today the root directory is a **container**: every entry inside it
  becomes a top-level module. To get `core::io`, `core` must therefore be a directory *inside*
  the root, and its own file must be `core/core.omg`. Flattening to
  `runtime/core/{core,io}.omg` under today's rule would produce two unrelated top-level
  modules, `core` and `io` — not `core::io`.

  The same container rule produces an asymmetry nobody chose: a library's package name is a
  module path segment (`core::io`), while an executable's is not (`simplemodule`, not
  `dev::simplemodule`). That asymmetry is a live bug, not just an inconsistency — see below.

- **Reasoning:**

  **The redundancy and the asymmetry have one cause.** Both fall out of "the root is a
  container." Making the root *be* the module fixes both at once, and it is the convention
  already used one level down: a directory-shaped module `X/` has its own file at `X/X.omg`.
  This change applies that same rule to the root instead of exempting it.

  **`internal` is broken for executables today.** Verified:

  ```omega
  # helper.omg
  internal shared() => i32 { 42 }
  # main.omg
  import helper::shared;
  main() => i32 { shared() }      # error: 'helper::shared' is not visible here
  ```

  `Driver::visibility_allows` implements package-wide `internal` as
  `declaring.first() == accessor.first()`. In an executable those are `helper` and `main` —
  different first segments, so the compiler considers them different packages. A library is
  fine only because everything under it shares the `core` segment. Under the new rule both
  become `dev::helper` and `dev::main`, and `internal` works. This is a bug fix that falls
  out of the layout change; it is not being pursued separately.

  **`main.omg` was never a compile-time concern.** Whether an object file becomes a library
  or a program is decided by the linker — `omgc` always emits a `.o`. The entry-module
  concept exists in the compiler to answer exactly one question, at exactly one line
  (`omega-codegen/src/cranelift/item.rs:94`): `path == entry && f.name == "main"` → emit the
  bare symbol `main` instead of a mangled one. Everything else — the `<name>.omg` /
  `<name>/<name>.omg` / `main.omg` probe in `omgc/main.rs`, `Driver::has_local_module`, the
  `entry` parameter threaded driver→codegen — exists only to feed that line.

  Keeping "a `main` declared in the root module is the C entry" preserves that one line while
  deleting the filename convention. Nothing declares itself a library: a package simply has
  no root-module `main`, so library-ness *emerges* instead of being a mode. That is what
  makes this consistent with the link-time model rather than merely compatible with it.

  Alternatives considered:
  - *Change only libraries; leave executables flat.* Rejected: it makes the convention
    conditional ("the root is the package module **unless** it is an executable"), keeps the
    asymmetry, and leaves `internal` broken for executables as a separate outstanding fix.
  - *Delete the entry concept entirely; require `@mangling(disabled)` on `main`.* Verified to
    work today — `@mangling(disabled)` already emits a bare symbol on a free function, so this
    would be pure subtraction with no new machinery. Rejected on ergonomics: every Omega
    program would carry an annotation above `main` that reads like an implementation detail,
    a papercut on the most common thing anyone writes.
  - *`main` anywhere in the package gets the bare symbol.* Rejected: a library with an
    internal helper named `main` would silently export the C entry symbol and only find out
    at a downstream link.
  - *Key the root file off the declared `--name=` rather than the directory basename.*
    Rejected: it would force `runtime/plat/libc/libc.omg` to be renamed `plat.omg`, defeating
    the point that the directory is honestly named `libc` and presents as `plat` purely
    through `--name=` (see `docs/22-platform-glue.md`).

- **Resolved concerns:**
  - **The rule is uniform** (decided): the root directory is always the package module, for
    libraries and executables alike.
  - **`main` in the root module is the C entry** (decided). `main.omg` as a filename ceases
    to mean anything.
  - **`core`, `std` and `plat` symbols do not change.** Their module paths are already
    `core::io`, `std::fmt`, `plat` — the *files* move, the paths do not. This is the plan's
    acceptance test, and it makes the runtime migration verifiable rather than merely
    plausible.
  - **`plat` needs no file move at all.** `runtime/plat/libc/libc.omg` is already
    `<root>/<basename>.omg`; today it happens to work as a flat child, and afterwards it is
    the root module. Same path, same symbols, zero edits.
  - **Relative imports need no source edits.** `relative_base_for` returns the module's own
    path when it is directory-shaped. `dev.omg` becomes directory-shaped (its `children_dir`
    is the root), so `import simplemodule;` resolves to `dev::simplemodule` with no change to
    the import statement. `core`'s internal imports are unaffected for the same reason.
  - **`examples/extern_lib/` must be renamed to `examples/mathlib/`.** Its directory basename
    (`extern_lib`) and its module file (`mathlib.omg`) disagree, which today is invisible
    because the root is a container and `--name=mathlib` matches the file. Under the new rule
    the root file must be `<basename>.omg`, so the directory is renamed and `--name=mathlib`
    becomes redundant.
  - **Executables' symbols change**, and only theirs: `examples/dev`'s items move from module
    `main` to module `dev`, and its siblings gain a `dev` segment. `main` itself keeps the
    bare symbol.

## Technical Details

### The rule

Given a root directory `<dir>/` with declared identity `N` (its basename, or `--name=`):

| path on disk | module |
|---|---|
| `<dir>/<basename>.omg` | `N` — the root module's own file |
| `<dir>/foo.omg` | `N::foo` |
| `<dir>/foo/` + `<dir>/foo/foo.omg` | `N::foo`, with children under it |
| *(no `<basename>.omg`)* | `N` exists as a namespace-only module |

A function named `main` declared in module `N` gets the bare C entry symbol. Anywhere else it
is mangled like any other function.

### What changes

**`omega-driver/src/fs_resolve.rs`** — `discover_tree` currently calls
`discover_into(root, &mut Vec::new(), out, None)`, which scans the root's *entries* and makes
each one a top-level module. It instead registers the root itself first:

- `own_file` = `<root>/<basename>.omg` if present, else `None`;
- `children_dir` = `Some(root)`;
- insert at path `[basename]`;
- then `discover_into(root, &mut vec![basename], out, Some(&basename))`.

The `skip` argument already exists for exactly this purpose one level down (it is what stops
`X/X.omg` being double-counted as both `X`'s own file and a child `X::X`), so the recursive
half needs no change at all.

`relabel_root` stays as-is: discovery keys off the on-disk basename, and `--name=` rewrites
the root segment afterwards. That is what keeps `libc/` presenting as `plat`.

**`omgc/src/main.rs`** — delete the entry probe. The entry module is always the declared
identity; there is no `main.omg` fallback and no `has_local_module` call. If
`Driver::has_local_module` has no other caller afterwards, delete it too.

**`omega-codegen/src/cranelift/item.rs:94`** — unchanged. `path == entry && f.name == "main"`
already says "the root module's `main`" once `entry` is the root module path.

**Runtime and examples** — file moves only:

| package | change |
|---|---|
| `runtime/core/core/*.omg` | → `runtime/core/*.omg` (7 files) |
| `runtime/std/std/*.omg` | → `runtime/std/*.omg` (13 files) |
| `runtime/plat/libc/` | none |
| `examples/dev/main.omg` | → `examples/dev/dev.omg` |
| `examples/extern_lib/` | → `examples/mathlib/`; drop `--name=mathlib` |
| `examples/io_demo/main.omg` | → `examples/io_demo/io_demo.omg` |
| `examples/{core_only,allocator_only,stdio_contract}/main.omg` | → `<dirname>.omg` |
| `examples/multi_print/app/main.omg` | → `app.omg`; `printlib/` unchanged |

**`justfile`** — `--name=std` becomes redundant (basename already matches);
`--extern=mathlib:examples/extern_lib/` becomes `--extern=mathlib:examples/mathlib/`. Every
`--extern=core:runtime/core/` is unchanged.

**Docs** — `10-modules-and-linkage.md` owns this convention and needs a real rewrite of its
"Module identity, and the entry module" and "Eager local discovery" sections, including the
`core`-lives-at-`runtime/core/core/core.omg` explanation, which stops being true.
`22-platform-glue.md` should state that `libc/` needs no move and why. `07-visibility.md`
gains the note that `internal` now works across an executable's modules.

### What must not change

- **`core`, `std`, `plat` symbols.** Byte-identical before and after; the acceptance test.
- **`relabel_root` and `--name=` semantics** — a directory may still be honestly named
  something other than its declared identity.
- **Import syntax and resolution.** No `import` statement in the tree changes.
- **The `<name>/<name>.omg` convention for non-root directory-shaped modules** — this change
  extends that rule upward, it does not replace it.
- **`--extern=<name>:<dir>`** spellings for `core`, `std`, `plat`.
- **Everything in `docs/14-known-issues.md`** — this plan fixes the `internal`-for-executables
  bug as a side effect and nothing else; do not fold in unrelated entries.

### Chosen approach

Land the discovery rule and the runtime move together, because they are mutually blocking:
the new rule breaks `runtime/core/core/core.omg` (the root would look for `core/core.omg`'s
*parent* to be the module), and the moved files do not resolve under the old rule. Examples
follow separately, since each is independently verifiable.

Symbol byte-identity for `core`/`std`/`plat` is what makes the risky half safe: if a single
symbol moves, the discovery rule is wrong, and that is detectable before any example is
touched.

### Risks and open questions

- **A namespace-only root.** A package with no `<basename>.omg` gets a root module with no
  own file. That is already a supported shape one level down, but it is newly reachable at
  the root, and it means `main` has nowhere to live. Decide whether that is an error at
  `omgc` level or simply a package with no entry point; the latter is consistent with
  library-ness being emergent.
- **`discover_tree`'s signature.** It currently takes only `root`. It needs the basename,
  which it can derive itself via `basename(root)` — but `basename` returns `Option`, and a
  root with no usable final component (`.`, `/`) currently fails later, in `omgc`. Keep the
  failure in one place; do not add a second path that silently produces an empty tree.
- **Two roots with the same basename.** `--extern=a:x/core --extern=b:y/core` already
  relabels both, so this is unchanged — but it is worth a test, since the root segment is now
  load-bearing for every module in the package rather than just the entry.
- **`is_core_module`** checks `path.first() == "core"` and is unaffected, but confirm it: it
  is the one place a module path's first segment carries special meaning.

## Testing

**New cases:**
- A package with a root module file and children: `<dir>/<dir>.omg` + `<dir>/foo.omg`
  resolving as `<dir>` and `<dir>::foo`.
- A package with **no** root module file — children still resolve under the namespace-only
  root.
- `internal` across two modules of one executable: the reproducer above must now compile.
  This is the bug fix and it has no test today.
- A nested directory-shaped module (`<dir>/foo/foo.omg` + `<dir>/foo/bar.omg`) still resolves
  as `<dir>::foo` and `<dir>::foo::bar` — the rule this change generalizes must keep working
  at its original depth.
- `--name=` on a package whose root file is named after the *directory*, not the declared
  name (`plat`): `runtime/plat/libc/libc.omg` still resolves as `plat`.
- A root-module `main` gets the bare symbol; a `main` in a **non-root** module of the same
  package gets a mangled one.

**Negative cases:**
- Both `<dir>/<basename>.omg` and `<dir>/<basename>/` present → `AmbiguousModule`, unchanged
  behaviour at the root.
- A package whose root file is named neither `<basename>.omg` nor anything else the rule
  accepts (the pre-rename `examples/extern_lib/` shape) → the file resolves as a *child*, and
  whatever referenced the package as a whole fails. Confirm the diagnostic names the module
  it could not find rather than failing obscurely later.

**Regression risk:**
- `nm target/core.o`, `target/std.o`, `target/plat.o` **byte-identical** before and after the
  runtime move. Capture baselines first.
- `tests/io_demo.expected`, `tests/stdio_contract.expected`, `tests/multi_print.expected`
  byte-identical; `just run-exec` exit 69.
- `examples/dev`'s symbols *do* change (module `main` → `dev`, siblings gain a `dev`
  segment). Diff `nm target/main.o` before/after and confirm every difference is that
  segment and nothing else.
- `compiler/omega-driver/tests/*.rs` build `TestPackage`s with a single `main.omg` and no
  externs. Under the new rule those become module `<temp-dir-basename>` rather than `main`,
  and `compile(&[Ident("main")])` no longer names a real module — the harness needs updating
  before anything else will run.

**Target coverage:**
- *Hosted:* every `just` recipe.
- *No-allocator / allocator-only:* `test-core-only`, `test-allocator-only` — both are
  single-file packages whose root file is already `<basename>.omg`-shaped after the rename,
  so they exercise the flat case.
