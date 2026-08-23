# Aliases

An `alias` gives an existing declaration a second source name. It is a
compile-time name indirection and nothing else: an alias never introduces a new
type identity, function identity, declaration, symbol, ABI entity, or runtime
value. Using a name through an alias must produce exactly the program that
spelling the target directly would produce.

## Declaration form

```omega
alias Short = VeryLongTypeName;
exposed alias Public = Hidden;
alias StringKeyedMap<V> = HashMap<*str, V>;
alias AB = spec A + B;
alias Dyn = *spec B + A;
```

An alias declaration is:

```text
[visibility] 'alias' Ident [ '<' GenericParams '>' ] '=' AliasTarget ';'
```

An alias is a **top-level item only**. It shall not appear inside a function
body, a block, an aggregate body, or any other nested position. It carries no
annotations.

`AliasTarget` is written with the ordinary type grammar. Two shapes are
distinguished:

- a **bare path** (`Name`, `module::Name`, `pkg::module::Name`), which may name
  any namespace: a module, a type, a spec, a function or overload set, a macro,
  or another alias;
- any **other type syntax** — primitives, pointers, arrays, function types,
  generic applications, `spec A + B`, `*spec A + B`.

Expression syntax is not a legal target. `alias A = 1 + 2;` and
`alias A = f();` are rejected as syntax errors.

An alias target uses ordinary path resolution from the alias declaration site,
including the explicit anchors `root::`, `self::`, and chained `super::` — the
same navigation any other path may write (see
[`modules-and-imports.md`](modules-and-imports.md)). Bring a module into scope
with an `import` and then name it, write an anchored path to navigate
relative to the alias declaration's own module, or write a fully qualified
top-level path such as `std::string::String`, which needs no separate import.

## What an alias may name

An alias may name:

- a type declaration (`struct`, `union`, `enum`, `marker`);
- a `spec` declaration;
- a function declaration or a whole overload set, including a
  function-valued `foreign` declaration;
- a macro;
- a module;
- another alias;
- any legal type expression.

An alias shall not name a value. A target that resolves to a global binding or
to a `comp` binding is invalid, as is a `gap` or any other declaration kind not
listed above. These are rejected at the alias declaration, whether or not the
alias is ever used.

## Resolution and identity

The target is resolved in the **module where the alias is declared**, not where
it is used. Using an alias therefore does not require the use site to be able
to spell the target's path itself.

The resolved identity is the target's own identity. In particular:

- `alias IntPair = Pair<i32, i32>;` makes `IntPair` and `Pair<i32, i32>` the
  same type: they accept each other with no conversion, have the same
  `sizeof`, the same fields and static methods, and the same symbol;
- `alias plus = add;` calls `add`; no wrapper function or extra symbol exists;
- an alias of an overloaded name forwards the complete candidate set visible at
  the alias declaration, and overload resolution still happens at the call
  site;
- an alias of a generic function forwards that function's generic inference,
  and instantiating through the alias materializes the target's own
  instantiation.

An alias chain preserves the final target's identity. Every alias in the chain
is still checked for its own validity and its own visibility.

## Generic aliases

A bare alias of a generic declaration forwards that declaration's generic
behavior — arity, defaults, bounds, and inference:

```omega
alias P = Pair;          # P<i32, i32> is Pair<i32, i32>
```

An alias written with its own generic parameter list is a **type alias
template**. Its parameters belong to the alias and are substituted into its
right-hand side:

```omega
alias Keyed<V> = Pair<*str, V>;      # Keyed<i32> is Pair<*str, i32>
alias Counted<T: Countable> = Holder<T>;
```

An alias template may fix, reorder, or nest type arguments. Its parameters may
carry bounds and defaults, which are validated like any other generic
parameters, but they create no nominal identity: `Keyed<i32>` *is*
`Pair<*str, i32>`.

Supplying the wrong number of type arguments for an alias template, or an
argument that does not satisfy an alias-owned bound, is an error reported
against the alias. A defaulted argument is bound and bound-checked the same
way whether the alias template is written in an ordinary type position or in
an item position such as aggregate construction or a static member access —
one arity/default rule applies everywhere an alias template may appear.

An explicitly generic alias names a type; its target must be a type or spec
expression.

## Spec conjunction aliases

`alias AB = spec A + B;` names a conjunction. It does not declare a spec, does
not create a conformance target named `AB`, and no type ever conforms *to*
`AB`.

A conjunction alias expands before contextual type rules apply, so it behaves
identically to the literal spelling in every position:

```omega
alias AB = spec A + B;

bounded<T: AB>(value: *T) => i32 { ... }   # same as <T: A + B>
static_param(value: AB) => i32 { ... }     # same as (value: spec A + B)

alias Dyn = *spec B + A;
dynamic_param(value: Dyn) => i32 { ... }   # a dynamic spec object
```

Canonical shape ordering and duplicate removal use the final spec identities,
never the alias spelling: `Dyn` above and `*spec A + B` are the same dynamic
object type. See [`specs-and-conformance.md`](specs-and-conformance.md).

## Visibility and re-export

An alias carries its own visibility and is its own visibility gate. Naming
something through an alias is checked in two steps:

1. the caller must be allowed to see the **alias**;
2. the target is then resolved and accessed with the **alias declaration
   module's** rights.

The caller is not required to satisfy the target declaration's own visibility a
second time. This makes re-export deliberate:

```omega
# in module `child`
struct Hidden { exposed value: i32; }

exposed alias Public = Hidden;
```

`child::Public` is usable from outside `child`, while `child::Hidden` is not.
The target declaration's own visibility is unchanged, and so are its members':
a hidden field or hidden method of `Hidden` remains hidden when reached through
`Public`.

`Public` may also be imported directly (`import child::Public;`), exactly like
an ordinary declaration; the alias's own visibility gates the import.

Conversely, an alias never widens anything by accident:

- a hidden alias of an exposed declaration is hidden, and is not importable
  from outside its declaration module;
- a **module** alias exposes only the module name. Traversing `Alias::Item`
  still checks `Item`'s own visibility. Re-exporting an item requires aliasing
  that item directly;
- an alias whose target is not visible from the alias declaration's own module
  is invalid.

An alias chain applies this rule at **every link**: each alias in the chain
must itself be visible from the module that names it, before its own target is
followed. A hidden alias may not be smuggled through by naming it from a
second, more permissive alias — the chain is only as re-exportable as its
least visible link. Once a chain has been validated this way, a caller that
may see the outermost alias reaches the final target with that outermost
alias's own resolved rights; the intermediate links' declaration modules are
not re-checked against the caller a second time.

An alias of an overloaded name applies the same rule to the whole candidate
set: the caller is gated once, against the alias, and the complete set of
overloads visible **from the alias's own declaration site** is then forwarded
as-is. A candidate's own individual visibility is not re-checked against the
external caller — that would either wrongly hide a candidate the alias's
declaration site can already see, or, for a non-alias import, still be
checked normally (see [`modules-and-imports.md`](modules-and-imports.md)).

`reveal` remains an explicit use-site bypass where it is written; aliasing is
not a standing bypass. See [`visibility.md`](visibility.md).

## Macro aliases

An alias may name a macro. The alias is a compile-time name binding only:
expansion uses the original macro's body and definition module, so hygiene and
definition-site name resolution are unchanged.

The **alias's** visibility is the effective visibility of the aliased macro for
dependency checks. An `exposed` alias of a macro whose body depends on a
narrower declaration is therefore rejected, exactly as an `exposed` macro with
that body would be. A macro alias target obeys the same per-link chain
visibility rule as any other alias: naming a hidden macro through a chain of
aliases requires every link, not just the outermost one, to be visible from
whichever module names it. See [`macros.md`](macros.md).

## Cycles

Alias cycles are always invalid, including cycles behind a pointer or any other
type constructor:

```omega
alias A = A;      # invalid
alias A = *A;     # invalid
alias A = B;
alias B = A;      # invalid
```

An alias creates no nominal cell, so the recursive-type indirection rule that
lets a `struct` contain a pointer to itself does not apply: there is nothing for
`*A` to point at. A cycle is reported with the resolution chain that closes it.

## Non-goals

The following are deliberately not part of this feature:

- local (statement-level) aliases;
- aliases of values, expressions, or compile-time constants;
- aliases of struct/union/enum members or enum variants;
- alias-generated wrapper functions or symbols;
- opaque type or newtype semantics — an alias is transparent by definition;
- a `spec` declaration-composition syntax such as `spec AB = A + B;`.
