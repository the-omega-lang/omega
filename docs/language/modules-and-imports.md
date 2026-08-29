# Modules and imports

Omega packages are directory trees. Module identity and source layout are part of the language/tooling contract because they determine paths visible to source and the stable names used across separately compiled packages.

## Package root and declared identity

A compilation is given a package root directory. Its declared root-module name defaults to that directory's basename. Tooling may override the declared identity (`omgc --name=<name>` for the local package and `--import=<name>:<dir>` for a dependency); source paths use the declared identity, not an independent alias layer.

Two package roots in one compilation context must not claim the same declared identity.

Every module identity -- a filesystem-discovered segment (an `.omg` file stem, or a directory segment that owns or leads to `.omg` source) as well as a declared identity (`--name=<name>`, an explicit `--import=<name>:<dir>`, or an inferred package/dependency root basename) -- must be a spelling the language's identifier grammar accepts as a single `Ident` token: it must not be a reserved keyword, and it must not be `root`, `self`, or `super`. Those three spellings remain ordinary contextual identifiers everywhere else (bindings, fields, parameters, item names); they are excluded only as module/package identities, because reusing one there would make the import anchor of the same spelling ambiguous with a literal module segment. A malformed or reserved candidate is a hard error, not a silently omitted module or a normalized substitute; Omega never rewrites `foo-bar` into `foo_bar` or otherwise reshapes an invalid name into a valid one. A directory that owns no Omega source, directly or in any descendant, is not a module candidate and is not subject to this rule regardless of its name.

A package root's own declared identity is validated where it is decided (an explicit `--name`/`--import` override, or the inferred physical basename), not by module discovery itself; declaring a valid override is the only way to compile a package whose physical directory basename is not spellable as an identifier.

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
import std::fmt::Display;
import self::sibling;
import self::mymodule::thing::something;
import root::simplemodule;
import super::helper;
import super::super::helper;

import thing::Thing as ImportedThing;
import thing::{ self, First, Second as Two, sub::{ Third, Fourth as Four } };
import reveal abc::{ A, sub::{ B, C } };
```

Grammar:

```ebnf
import       = "import", [ "reveal" ],
               [ "root", "::" | "self", "::" | { "super", "::" } ],
               path, import-tail,
               ";" ;

import-tail  = [ "as", identifier ]
             | "::", import-group ;

import-group = "{", import-entry, { ",", import-entry }, [ "," ], "}" ;

import-entry = [ "reveal" ],
               ( "self", [ "as", identifier ]
               | identifier, { "::", identifier }, import-tail ) ;
```

`root::`, `self::`, and chained `super::` are not import-only syntax: they are
explicit anchors of the ordinary `Path` grammar itself, and so are legal
wherever a path is legal -- a type position (including nested inside pointer,
array, or generic-argument syntax), an expression, a function type, an alias
target, or a macro body. An import's source path is one such anchored path,
anchored by one of four mutually exclusive forms:

- **Unprefixed (top-level).** The path is already an absolute logical path whose first segment must be a known top-level package identity: either the package that owns the importing module, or a dependency registered with the compilation. There is no relative fallback -- a missing head is an unknown-top-level-package error, not an invitation to search locally.
- **`root::`.** Starts at the root of the package that owns the importing module, independent of how deeply the importing module is nested. When source belonging to a registered dependency is itself being analyzed, `root::` refers to that dependency's own root, never the consuming package's root.
- **`self::`.** Starts at the exact logical module being analyzed, independent of whether that module is file-shaped or directory-shaped.
- **`super::`, one or more times.** Each occurrence removes one logical module segment from the importing module's own path before appending the source path. A chain may not remove the importing module's package-root segment; `super::` at the package root, or a chain that would cross it, is a deterministic compile error.

`root`, `self`, and `super` are reserved only in these anchor positions; as ordinary path segments (including the final item name) or in any other identifier position, they remain the same contextual identifiers described in [`lexical-structure.md`](lexical-structure.md).

The **unprefixed, top-level-by-default** reading above is specific to imports. An unanchored path written anywhere else (a type position, an expression, an alias target) keeps its own ordinary relative lookup rules instead -- it is not implicitly top-level. Only a path that actually writes `root::`, `self::`, or `super::` gets the anchored meaning described here, in any position.

Importing an item binds its final name in the importing module. Importing a module makes that module path/name available according to normal resolution rules.

### An import's target is resolved whether or not it is used

Every binding an import creates is checked when the import is processed, not
when the bound name is first referenced. A binding whose target names nothing,
or names something the importing module may not reach, is a compile error even
if the module never uses it. Deleting the last use of an import does not turn a
broken import into a legal one.

What an import establishes about its target is that the target exists and is
nameable here. It does not force the target's own analysis: whether the
imported declaration is itself well formed remains a question its own module
answers, and one a use site asks. Two modules may therefore import from each
other freely; only a genuinely circular *definition* is an error.

A failing import is reported once, at the import that failed, and not again at
each use of the name it bound.

## Import trees

An `import` is an ordered tree: a source path prefix, optionally followed by a
brace group of entries that extend that prefix, nested to any depth. Only the
tree's *terminal* entries import anything; interior entries merely extend the
prefix. A tree is exactly equivalent to writing each of its terminal bindings
as its own `import` statement, in textual depth-first leaf order:

```omega
import thing::{ First, Second as Two, sub::{ Third, Fourth as Four } };
```

means the same as

```omega
import thing::First;
import thing::Second as Two;
import thing::sub::Third;
import thing::sub::Fourth as Four;
```

A group must attach to a prefix with at least one written segment. `root::`,
`self::`, and `super::` are anchors of the prefix, not prefixes of their own,
so `import self::{ ... }` is not valid while `import self::thing::{ ... }` is.
A group must contain at least one entry.

### Renaming with `as`

`as <identifier>` changes only the name the importing module binds. It never
changes the target's identity, visibility, or symbol, and it is not a
re-export: `import thing::Thing as ImportedThing;` resolves `thing::Thing` and
claims `ImportedThing` alone, leaving `Thing` free for the importing module to
use for something else.

`as` renames a terminal binding only. An interior prefix cannot be renamed;
a prefix is renamed by importing it through a `self` entry instead.

### The `self` entry

Inside a group, `self` is a terminal entry that binds the *enclosing prefix*
itself. `import thing::{ self, Thing };` imports the module `thing` under its
ordinary final name and also imports `thing::Thing`, and
`import thing::{ self as TheModule };` binds that same prefix as
`TheModule`. This `self` is an import-tree entry, not a path segment or an
anchor: leading `self::` keeps its ordinary anchor meaning, `import self;` is
unchanged, and `self` may not be followed by further segments or a group.

### Scoped `reveal`

`reveal` may be written on the whole import or on any group entry, including
a terminal one. A terminal binding's effective `reveal` is the logical OR of
every `reveal` written from the root of the tree down to it, so revealing a
subtree deliberately reveals every binding beneath it:

```omega
import reveal abc::{ A, sub::{ B, C } };   # A, sub::B and sub::C are all revealed
import abc::{ reveal A, B };               # only A is revealed
import abc::{ reveal sub::{ A, B }, C };   # sub::A and sub::B, but not C
```

Interior entries create no import of their own, so `reveal` on a prefix means
only that the bindings beneath it inherit the bypass. It says nothing about
the prefix itself: physical modules carry no visibility modifier and there is
nothing about a module to reveal. Writing `reveal` where the bypass is not
needed is legal and simply redundant, including when it is written twice on
one branch.

### Each binding is independent

Every terminal binding is its own import for target resolution, name claiming,
collision reporting, unused-import tracking, and diagnostics. One failing
binding neither suppresses nor merges its siblings, and it still claims the
name it bound. Diagnostics point at the entry that caused them rather than at
the whole statement.

Annotations are written on the `import` item, not on entries, so an annotation
such as `@suppress(unused_import)` applies to every binding that item
produces.

Imports do not automatically re-export what they import. Re-export is a separate, deliberate act: an `alias` declaration binds a name of its own, with its own visibility, to an existing declaration or module, and an `exposed alias` therefore makes its target nameable from outside the declaring module. An alias target resolves at the alias declaration site, so it does not need a matching import. An alias name may itself be imported directly (`import module::SomeAlias;`), exactly like an ordinary declaration; the alias's own visibility gates the import, and the imported name still resolves through the alias's own semantics. See [`aliases.md`](aliases.md) and [`visibility.md`](visibility.md).

A **module** alias re-exports only the module name; traversing it still checks each named item's own visibility.

`import reveal ...` applies the visibility-bypass semantics defined in [`visibility.md`](visibility.md).

## External packages

An unprefixed import whose first segment names a registered dependency (`omgc --import=<name>:<dir>`) resolves against that dependency's own module tree, the same top-level namespace the local package's own identity occupies. External package modules are separately compiled objects/packages; source-level resolution must agree on package/module identity across processes.

External dependencies are not textually source-included into the local package. Their declarations/signatures participate in name and type resolution through Omega's separate-compilation model.

## `core` ambient names

The package declared as `core` is special: its exposed declarations participate in ambient/prelude-style name resolution. This permits primitive/core facilities to be used without explicit imports where the core prelude makes them available.

This ambient lookup is a fallback, not license for arbitrary dependency names to become global. Ordinary external packages require imports.

Exposed macros in `core` participate in the same ambient fallback. Imported macros bind their ordinary invocation name; qualified macro invocation syntax is not required.

Because ambient `core` names are a fallback, an explicit binding in the same namespace hides them. A module-scope declaration, alias, or import named `Result` is the `Result` that module means, and an imported macro replaces an ambient `core` macro of the same invocation name. Hiding an ambient name does not alter the underlying `core` declaration: the qualified path (`core::result::Result`) keeps resolving wherever visibility permits.

## Explicit module-scope names are not shadowable

Module scope has no source-order shadowing. Within one namespace of one module, declarations, aliases, and imports all make an *explicit* claim on a name -- an import claims the name it binds locally, which `as` renames, and two explicit claims on the same name are a redeclaration regardless of which forms are involved or which order they appear in. The later claim is the one reported. Function overloads remain the only same-name exception among ordinary declarations, and macros have their own separate namespace in which the same rule applies among explicit macro definitions, macro aliases, and macro imports.

An import claims its bound name whether or not its target resolves, so a failed import does not silently hand the name to a competing declaration.

## Name resolution and visibility

Resolution must preserve declaration identity, not merely textual names. Imported and local declarations with the same spelling must not become accidentally interchangeable if their module/spec identity differs.

Visibility is checked after resolving the declaration and is governed by [`visibility.md`](visibility.md). `reveal` is an explicit bypass at the source position where it is written; it does not permanently change a declaration's visibility.

## Determinism

For identical source/package identities and target configuration, name resolution and generated external symbol identity must be deterministic. Independently compiled packages must agree on the identities required for linking; filesystem enumeration order, map iteration order, or process randomness must not affect them.
