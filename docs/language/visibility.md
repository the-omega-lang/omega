# Visibility

This chapter is normative for current Omega language behavior. Known implementation limitations are tracked under [`../issues/`](../issues/).

```omega
exposed struct Public { ... }
shared struct PackageWide { ... }
struct HiddenByDefault { ... }

reveal some_module::hidden_thing();
import reveal extern::some_package;
```

Omega has three declaration visibility levels plus a use-site bypass:

- `exposed`: visible from any package.
- `shared`: visible anywhere in the same top-level package.
- no modifier (`hidden`): narrowest visibility; the exact scope depends on whether the declaration is a top-level item or a member.
- `reveal`: explicitly bypasses an otherwise-applicable visibility restriction at a particular use site.

`exposed`, `shared`, and `reveal` are contextual syntax rather than globally reserved words.

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
import reveal extern::lib::hidden_module;
```

The bypass applies only to the syntactic use wrapped by `reveal`; it does not change the declaration itself or grant permanent visibility to later code.

An implementation may warn when a `reveal` was unnecessary. Known current edge cases in propagation through complex place expressions are tracked in [`../issues/design-debt.md`](../issues/design-debt.md).

## Macros

A macro body may not use declarations less visible than the macro itself in a way that would expose them to callers. In particular, an `exposed` macro cannot smuggle a `shared` or hidden dependency across package boundaries. Caller-side `reveal` does not retroactively weaken the macro definition's own visibility obligations.

## Specs and conformance

A function requirement declared inside a spec has no independent visibility modifier; its effective visibility is the declaring spec's visibility.

A function body written in a `conform` block likewise has no visibility modifier and inherits the matched requirement's effective visibility.

```omega
shared spec Mammal {
    breathe(*self) => i32;
}

struct Dog {}
conform Dog to Mammal {
    breathe(*self) => i32 { 1 }
}
```

Each requirement keeps the visibility of the spec that declared it, including when reached through a spec alias/conjunction.

Dynamic dispatch must not widen visibility. A method that is inaccessible to a source location through direct dispatch must not become callable there merely by coercing the value to `spec *S`; forming/using the dynamic object remains subject to the requirement's effective visibility.
