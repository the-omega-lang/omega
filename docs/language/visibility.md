# Visibility

This chapter is normative for current Omega language behavior. Known implementation limitations are tracked under [`../issues/`](../issues/).

```omega
exposed struct Public { ... }
shared struct PackageWide { ... }
struct HiddenByDefault { ... }
hidden struct AlsoHiddenByDefault { ... }

reveal some_module::hidden_thing();
import reveal some_module::hidden_thing;
```

Omega has three declaration visibility levels plus a use-site bypass:

- `exposed`: visible from any package.
- `shared`: visible anywhere in the same top-level package.
- `hidden`, or no modifier: visible throughout the exact declaring module (one source file), for top-level items and members alike.
- `reveal`: explicitly bypasses an otherwise-applicable visibility restriction at a particular use site.

`exposed`, `shared`, `hidden`, and `reveal` are contextual syntax rather than globally reserved words.

`hidden` is written out only where it changes something -- most declarations already default to hidden, so writing it there is redundant and an implementation may warn (see "Spec member visibility" below for the one case where it is not redundant).

## Binary visibility is a separate decision

Everything in this chapter is **source** visibility: which Omega source may name a declaration. It says nothing about the symbol the compiler emits.

Whether an emitted symbol is visible outside the linked image it belongs to is decided by `@symbol(export)` and by whether the item is `foreign` (see [`annotations-and-sizeof.md`](annotations-and-sizeof.md#export)). The two kinds of visibility are independent in both directions: an `exposed` Omega item is not exported from a shared image unless it says `export`, and an exported item that is `hidden` in source is still unreachable from other Omega source without `reveal`.

`reveal` bypasses an Omega access check. It is not a linkage mechanism and cannot reach a symbol that a different shared image did not export.

## Hidden items and hidden members

`hidden` has one meaning for every declaration kind: the declaration is visible throughout its exact declaring module and invisible from every other module without `reveal`.

For a field or an inherent method, the declaring module is the module that declares its struct/union/enum/marker owner. Any code in that module -- free functions, struct literals, `meet` bodies, other types' methods -- may use the owner's hidden members. To seal a type's fields completely, declare the type alone in its module.

## `shared`

`shared` is package-wide, not descendant-module-only. Any module within the same top-level package may access a shared item/member subject to the normal kind-specific lookup rules.

## `reveal`

`reveal` is an explicit expression/use-site visibility bypass:

```omega
reveal base.hidden_method();
reveal value.hidden_field = 10;
p := &mut reveal value.hidden_field;
```

It can also be used in imports, where it authorizes binding a name the
importing module could not otherwise reach:

```omega
# `lib::Secret` is hidden; this import is what carries the bypass, and the
# bound name stays usable afterwards.
import reveal lib::Secret;
```

An import is a tree, and `reveal` is scoped to the node it is written on: a
terminal binding's effective bypass is the logical OR of every `reveal` from
the root of the tree down to it. Revealing a subtree therefore reveals every
binding beneath it, and nothing beside it:

```omega
import lib::{ reveal Secret, Public };            # only `Secret` is revealed
import lib::{ reveal internals::{ A, B }, C };    # `A` and `B`, but not `C`
```

Interior nodes of an import tree bind nothing, so `reveal` on a prefix says
only that the bindings beneath it inherit the bypass. Physical modules have no
visibility modifier and nothing about them is revealed; see
[`modules-and-imports.md`](modules-and-imports.md).

The bypass applies only to the syntactic use wrapped by `reveal`; it does not change the declaration itself or grant permanent visibility to later code.

An implementation may warn when a `reveal` was unnecessary. Known current edge cases in propagation through complex place expressions are tracked in [`../issues/design-debt.md`](../issues/design-debt.md).

## Aliases and re-export

An `alias` carries its own visibility and is its own gate. A caller must be allowed to see the alias; the target is then resolved with the alias declaration module's rights, and the caller does not have to satisfy the target declaration's visibility again. `exposed alias Public = Hidden;` is therefore a deliberate capability transfer, while a hidden alias of an exposed declaration stays hidden.

An alias changes nothing about its target: the target declaration's own visibility, its members' visibility, its spec members' visibility, and its symbol identity are unaffected. An alias whose target is not visible from the alias declaration's own module is invalid. Full rules are in [`aliases.md`](aliases.md).

## Macros

A macro's `hidden`/`shared`/`exposed` modifier says who may name and invoke the macro. It says nothing about what the macro body may reach: every visibility-bearing reference the body writes is checked as ordinary code written in the module the macro was **defined** in, exactly like the name resolution of that same reference. An `exposed` macro may therefore wrap a hidden helper of its own module or a `shared` package API, just as an `exposed` function may.

```omega
# In `core::panic`. `PanicHandler::panic` is `shared` to `core`, and the
# macro body is authored inside `core::panic`, so it may call the handler
# while application source may only reach it through `panic$`.
exposed macro panic($message: expr...) => {
    { ... PanicHandler::panic(&info) }
}
```

Syntax substituted by the caller keeps the caller's origin and is checked with the caller's rights. This cuts both ways for `reveal`: a `reveal` authorizes only references sharing its own origin, so a caller's `reveal some_macro$()` cannot reach anything inside the expansion, and a `reveal` the macro body writes cannot reach into the caller's substituted arguments. A macro body that needs a declaration genuinely invisible at its definition site writes its own `reveal`; a body that reaches something it is not allowed to see and does not reveal it fails with the ordinary visibility error for the rule it broke.

Member names follow the same rule: a macro-authored name for a field or method is checked with the definition module's rights, whatever module the expansion lands in. A macro defined in a type's own module may therefore name the type's `hidden` members without `reveal`; one defined elsewhere needs a `reveal` the body writes itself.

The same rules apply to a gap function named from a macro body.

An alias of a macro re-exports the name; the alias's visibility gates who may invoke it, while expansion still uses the original definition site's rights.

## Specs and conformance

A function requirement declared inside a spec may carry an explicit visibility modifier (`hidden`, `shared`, or `exposed`); when omitted, it defaults to the declaring spec's own visibility -- unlike every other declaration kind, whose default is always `hidden`. An explicit modifier must not exceed the spec's own visibility.

```omega
shared spec Greeter {
    name(*self) => i32;

    # No modifier: inherits the spec's own visibility (`shared`).
    greet(*self) => i32 {
        self.double_name() + 1
    }

    # Narrower than the spec: reachable throughout the spec's module,
    # e.g. from `greet`'s default body above, but not from other modules
    # of the package. Writing `hidden` is not redundant here, since the
    # spec's own default is `shared`, not `hidden`.
    hidden double_name(*self) => i32 {
        self.name() * 2
    }
}
```

Writing a modifier greater than the spec's own visibility (e.g. `exposed` on a member of a `shared spec`) is a compile error: a spec member can never be more visible than the spec that declares it.

A function body written in a `meet` block has no visibility modifier of its own and inherits the matched requirement's effective visibility.

```omega
shared spec Mammal {
    breathe(*self) => i32;
}

struct Dog {}
meet Mammal for Dog {
    breathe(*self) => i32 { 1 }
}
```

Each requirement keeps the visibility of the spec that declared it (or its own explicit modifier, capped at the spec's visibility), including when reached through a conjunction (`spec A + B`).

A requirement's declaring module is the **spec's** module, never the module of the conforming type or of the `meet`. Its visibility is checked against that module at every call form: method-call syntax through a generic bound, `Spec::name(value)`, fully qualified spec calls, and dynamic dispatch.

Defining is not using. A `meet` anywhere the conformance rules allow may supply a body for a requirement it cannot call. This is the pattern for a hook that only the spec's own module calls:

```omega
# In module `counting`.
exposed spec Counter {
    count(*self) => i32;

    hidden scaled(*self) => i32 {
        self.count() * 2
    }
}

exposed total<T: Counter>(value: *T) => i32 {
    value.scaled() + 1
}
```

Another module may write `meet counting::Counter for Tally { ... }` and pass a `Tally` to `counting::total`, but calling `scaled` on it from there is a visibility error without `reveal`.

Dynamic dispatch must not widen visibility. A method that is inaccessible to a source location through direct dispatch must not become callable there merely by coercing the value to `*spec S`; forming/using the dynamic object remains subject to the requirement's effective visibility.
