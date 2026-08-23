# Visibility

This chapter is normative for current Omega language behavior. Known implementation limitations are tracked under [`../issues/`](../issues/).

```omega
exposed struct Public { ... }
shared struct PackageWide { ... }
struct HiddenByDefault { ... }
hidden struct AlsoHiddenByDefault { ... }

reveal some_module::hidden_thing();
import reveal some_package;
```

Omega has three declaration visibility levels plus a use-site bypass:

- `exposed`: visible from any package.
- `shared`: visible anywhere in the same top-level package.
- `hidden`, or no modifier: narrowest visibility; the exact scope depends on whether the declaration is a top-level item or a member.
- `reveal`: explicitly bypasses an otherwise-applicable visibility restriction at a particular use site.

`exposed`, `shared`, `hidden`, and `reveal` are contextual syntax rather than globally reserved words.

`hidden` is written out only where it changes something -- most declarations already default to hidden, so writing it there is redundant and an implementation may warn (see "Spec member visibility" below for the one case where it is not redundant).

## Hidden items and hidden members

A hidden top-level item is visible throughout its exact declaring module.

A hidden field or method is narrower: it is visible only from methods of the exact declaring struct/union/enum/marker owner. An unrelated free function in the same module cannot access that hidden member without `reveal`.

## `shared`

`shared` is package-wide, not descendant-module-only. Any module within the same top-level package may access a shared item/member subject to the normal kind-specific lookup rules.

## `reveal`

`reveal` is an explicit expression/use-site visibility bypass:

```omega
reveal base.hidden_method();
reveal value.hidden_field = 10;
p := &mut reveal value.hidden_field;
```

It can also be used in imports:

```omega
import reveal lib::hidden_module;
```

The bypass applies only to the syntactic use wrapped by `reveal`; it does not change the declaration itself or grant permanent visibility to later code.

An implementation may warn when a `reveal` was unnecessary. Known current edge cases in propagation through complex place expressions are tracked in [`../issues/design-debt.md`](../issues/design-debt.md).

## Aliases and re-export

An `alias` carries its own visibility and is its own gate. A caller must be allowed to see the alias; the target is then resolved with the alias declaration module's rights, and the caller does not have to satisfy the target declaration's visibility again. `exposed alias Public = Hidden;` is therefore a deliberate capability transfer, while a hidden alias of an exposed declaration stays hidden.

An alias changes nothing about its target: the target declaration's own visibility, its members' visibility, its spec members' visibility, and its symbol identity are unaffected. An alias whose target is not visible from the alias declaration's own module is invalid. Full rules are in [`aliases.md`](aliases.md).

## Macros

A macro body may not use declarations less visible than the macro itself in a way that would expose them to callers. In particular, an `exposed` macro cannot smuggle a `shared` or hidden dependency across package boundaries. Caller-side `reveal` does not retroactively weaken the macro definition's own visibility obligations. An alias of a macro is checked with the **alias's** visibility, so an `exposed` alias of a hidden macro carries the same obligation.

## Specs and conformance

A function requirement declared inside a spec may carry an explicit visibility modifier (`hidden`, `shared`, or `exposed`); when omitted, it defaults to the declaring spec's own visibility -- unlike every other declaration kind, whose default is always `hidden`. An explicit modifier must not exceed the spec's own visibility.

```omega
shared spec Greeter {
    name(*self) => i32;

    # No modifier: inherits the spec's own visibility (`shared`).
    greet(*self) => i32 {
        self.double_name() + 1
    }

    # Narrower than the spec: only reachable from other methods of this
    # same spec, e.g. `greet`'s default body above. This is the one case
    # where writing `hidden` is not redundant, since the spec's own
    # default here is `shared`, not `hidden`.
    hidden double_name(*self) => i32 {
        self.name() * 2
    }
}
```

Writing a modifier greater than the spec's own visibility (e.g. `exposed` on a member of a `shared spec`) is a compile error: a spec member can never be more visible than the spec that declares it.

A function body written in a `conform` block has no visibility modifier of its own and inherits the matched requirement's effective visibility.

```omega
shared spec Mammal {
    breathe(*self) => i32;
}

struct Dog {}
conform Dog to Mammal {
    breathe(*self) => i32 { 1 }
}
```

Each requirement keeps the visibility of the spec that declared it (or its own explicit modifier, capped at the spec's visibility), including when reached through a conjunction (`spec A + B`).

Dynamic dispatch must not widen visibility. A method that is inaccessible to a source location through direct dispatch must not become callable there merely by coercing the value to `*spec S`; forming/using the dynamic object remains subject to the requirement's effective visibility.
