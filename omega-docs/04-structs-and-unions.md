# Structs & unions

## Structs

```
struct Vec2 {
    exposed x: i32;
    exposed y: i32;

    exposed origin() => Vec2 { Vec2 { x = 0; y = 0; } }        # static
    exposed translate(*mut self, dx: i32, dy: i32) => void {    # member
        self.x += dx;
        self.y += dy;
    }
}
```

Fields and methods live in the same brace block, `;`-separated. A function
declared **without** a self-mode parameter is *static* (called
`Type::function(...)`, no instance); one **with** `self`/`mut self`/`*self`/
`*mut self` is an ordinary member function called on an instance. See
[variables & mutability](02-variables-and-mutability.md) for the four
self-forms and their call-site auto-adaptation, and
[visibility](07-visibility.md) for per-field/per-method `exposed`/
`internal`/private modifiers.

Struct-literal fields use `name = value;` (not `:` — `:` stays reserved for
declarations, avoiding any ambiguity with a `struct` statement's own field
list). A literal must set every field exactly once; there is no partial
initialization or `..Default::default()`-style spread.

**Structs support single-generic-parameter-list generics**
(`struct MyNode<T> { value: T; next: *MyNode<T>; }`) and can implement
[specs](08-specs.md) via `struct S : Spec1, Spec2 { ... }`.

## Unions

```
union Value {
    exposed as_i32: i32;
    exposed as_f32: f32;
}
```

Real C-style unions: every field overlaps the same storage (offset 0, sized
to the largest member). Constructing one sets **exactly one** field
(`Value { as_i32 = 42; }` — not "every field," unlike a struct literal).

Unions are a **deliberately separate parallel item pipeline**, not a
generalization shared with `struct` — this mirrors the pre-existing
precedent that `enum` is already its own pipeline alongside `struct` rather
than a unified "product/sum type" abstraction. Every struct-shaped
touchpoint (lexer keyword, AST/HIR/resolved-type node, driver cell map,
codegen construction) is mirrored by hand for unions rather than factored
through a shared abstraction. This is a conscious style choice in this
codebase: three item kinds have now gone through this "mirror, don't
unify" treatment (struct, enum, union), and it's treated as the established
pattern for a fourth, not a smell to eventually refactor away.

Unions do **not** support `@layout` (no `pack`/`align` — always alignment
`1`, matching their pre-annotation layout exactly) and have no notion of
per-variant anything, unlike enums.

## Layout & `sizeof`

See [primitives](01-primitives.md) and [annotations](09-annotations.md) for
the full packed-by-default layout model, `@layout(pack = ..., align = ...)`,
and the `sizeof<Type>` expression.

## Caveats

- **A generic struct with a `self`-using method fails at its own
  declaration** with a confusing `'Pair' expects 1 type argument(s), found
  0` error, both locally and across `--extern`. No pre-existing example in
  the repo exercised this shape before it was found (existing generic
  structs had fields only). Likely a `Self`/self-type substitution bug in
  generic-owner method-signature resolution. See
  [generics](06-generics.md) for the full list of generic-related gaps.
- **Generic type inference can't deduce `T` from a generic-struct-typed
  argument** (`sum_pair<T>(p: Pair<T>)` called with a `Pair<i32>` fails to
  infer `T`) — `unify_generic_type` has no case for a generic struct/enum
  argument at all, only plain named/pointer/array/function shapes.
