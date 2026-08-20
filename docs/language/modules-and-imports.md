# Modules and imports

Omega packages are directory trees. Module identity and source layout are part of the language/tooling contract because they determine paths visible to source and the stable names used across separately compiled packages.

## Package root and declared identity

A compilation is given a package root directory. Its declared root-module name defaults to that directory's basename. Tooling may override the declared identity (`omgc --name=<name>` for the local package and `--extern=<name>:<dir>` for a dependency); source paths use the declared identity, not an independent alias layer.

Two package roots in one compilation context must not claim the same declared identity.

Every module identity -- a filesystem-discovered segment (an `.omg` file stem, or a directory segment that owns or leads to `.omg` source) as well as a declared identity (`--name=<name>`, an explicit `--extern=<name>:<dir>`, or an inferred package/extern root basename) -- must be a spelling the language's identifier grammar accepts as a single `Ident` token: it must not be a reserved keyword. A malformed candidate is a hard error, not a silently omitted module or a normalized substitute; Omega never rewrites `foo-bar` into `foo_bar` or otherwise reshapes an invalid name into a valid one. A directory that owns no Omega source, directly or in any descendant, is not a module candidate and is not subject to this rule regardless of its name.

A package root's own declared identity is validated where it is decided (an explicit `--name`/`--extern` override, or the inferred physical basename), not by module discovery itself; declaring a valid override is the only way to compile a package whose physical directory basename is not spellable as an identifier.

The root directory itself is the root module. It may have its own source file named after the directory's **physical basename**:

```text
foo/foo.omg       # root module `foo`
foo/bar.omg       # child module `foo::bar`
foo/baz/qux.omg   # child module path according to directory-shaped rules
```

A root may be namespace-only and contain children without an own source file.

`main.omg` has no special module meaning. Entry-point behavior is described in [`foreign-function-interface.md`](foreign-function-interface.md).

## Directory-shaped modules

A directory-shaped module may own a source file named after the directory and children beneath that directory. Module discovery must reject ambiguous filesystem layouts where the same logical module is claimed by incompatible file/directory shapes rather than silently selecting one.

A known edge case in the current implementation's same-name-directory discovery is tracked in [`../issues/known-issues.md`](../issues/known-issues.md); it is not normative behavior.

## Local package membership

Every module discovered under the local package root is part of that package's compilation set, whether or not another local module imports it. Imports are required to *name/reference* declarations, not to decide whether a local source file belongs to the package.

Consequently, a malformed local module cannot become valid merely because no other module imports it.

## Import syntax

```omega
import sibling;
import mymodule::thing::something;
import root::simplemodule;
import extern::std::fmt::Display;
```

Grammar:

```ebnf
import = "import", [ "reveal" ],
         [ "root", "::" | "extern", "::" ],
         path,
         ";" ;
```

An unrooted import is resolved relative to the current package/module rules. `root::` explicitly starts at the current package root. `extern::NAME::...` starts at a registered external package identity.

Importing an item binds its final name in the importing module. Importing a module makes that module path/name available according to normal resolution rules.

Imports do not automatically re-export what they import. Omega currently has no `pub use`-style re-export mechanism; see [`visibility.md`](visibility.md).

`import reveal ...` applies the visibility-bypass semantics defined in [`visibility.md`](visibility.md).

## External packages

An `extern::name::...` path can resolve only if a dependency with declared identity `name` was supplied to the compilation. External package modules are separately compiled objects/packages; source-level resolution must agree on package/module identity across processes.

External dependencies are not textually source-included into the local package. Their declarations/signatures participate in name and type resolution through Omega's separate-compilation model.

## `core` ambient names

The package declared as `core` is special: its exposed declarations participate in ambient/prelude-style name resolution. This permits primitive/core facilities to be used without explicit imports where the core prelude makes them available.

This ambient lookup is a fallback, not license for arbitrary dependency names to become global. Ordinary external packages require imports.

Exposed macros in `core` participate in the same ambient fallback. Imported macros bind their ordinary invocation name; qualified macro invocation syntax is not required.

## Name resolution and visibility

Resolution must preserve declaration identity, not merely textual names. Imported and local declarations with the same spelling must not become accidentally interchangeable if their module/spec identity differs.

Visibility is checked after resolving the declaration and is governed by [`visibility.md`](visibility.md). `reveal` is an explicit bypass at the source position where it is written; it does not permanently change a declaration's visibility.

## Determinism

For identical source/package identities and target configuration, name resolution and generated external symbol identity must be deterministic. Independently compiled packages must agree on the identities required for linking; filesystem enumeration order, map iteration order, or process randomness must not affect them.
