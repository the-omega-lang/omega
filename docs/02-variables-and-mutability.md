# Variables & mutability

## Declaration forms

```
a : i32;            # declared, uninitialized
a : i32 = 10;        # declared with initializer
a := 10;              # inferred (walrus)
mut a : i32 = 10;      # mutable, explicit type
mut a := 10;             # mutable, inferred
```

Everything is **immutable unless explicitly marked `mut`** — a binding, a
pointer's pointee (`*mut T` vs. plain `*T`), or a method's `self` (`*mut
self`/`mut self` vs. `*self`/`self`). This is a deliberate, late addition to
the language (added after enums/pattern-matching already existed) that
closed a real soundness gap — see below.

`mut` and `:=` are **contextual keywords**, recognized by comparing an
already-lexed identifier's text, the same way `self`/`type`/`root`/`hidden`
are — not reserved words, so none of them ever collide with a user
identifier of the same spelling.

## Scope, deliberately narrow

Only **local/global bindings** and **pointer pointee-mutability** get a
mutability choice. Function parameters — including struct/enum fields,
which share the exact same declaration grammar as parameters — are
**always immutable bindings**, no `mut` recognized there at all. `self` is
the one partial exception: only its *pointee* type varies (`mut self`/`*mut
self`), never the `self` binding itself directly (see below for how `mut
self` is actually implemented). `extern` declarations are also always
immutable.

## `self`'s four forms

```
self       # by-value copy, immutable local
mut self    # by-value copy, mutable local (never affects the caller)
*self         # pointer to the caller's value, immutable pointee
*mut self       # pointer to the caller's value, mutable pointee
```

The call site adapts automatically in every direction: a pointer-shaped
method called on a plain value auto-refs; a value-shaped method called on a
pointer auto-derefs-and-copies. `mut self` desugars to an *implicit shadow
local* — the synthesized parameter stays an ordinary immutable-by-value
`self`, and the method body's first statement is a synthetic `mut self :=
self;` — reusing the pre-existing "shadow a parameter to vary it" idiom
rather than teaching codegen that a parameter slot can be mutable.

Specs reject by-value `self` outright (`SpecSelfMustBePointer`) — dynamic
dispatch erases `Self` to zero IR leaves for the vtable call's signature,
and a by-value self needs a real size to copy from that no longer exists
once erased.

## The soundness fix mutability exists for

Before mutability, enum variant refinement (see
[enums & pattern matching](05-enums-and-pattern-matching.md)) had a real
aliasing hole: `&`-ing a refined binding (`p := &a` where `a :
Entity::Person`) let a callee write a *different* variant through `p`,
leaving `a`'s own tracked refinement silently stale. The fix:

1. `ResolvedType::accepts`'s refined→plain pointer-widening rule only fires
   for **immutable** pointers now — nothing can write a different variant
   through one regardless, so widening it is always safe.
2. `&mut place` **always** produces a fully widened (de-refined) pointee —
   no narrowed-aware exception, unlike plain `&`.
3. `&mut place` on a bare-identifier binding also widens *that binding's
   own* tracked type from that point forward (`Context::widen_variable`) —
   code after `p := &mut a` that reads `a` directly no longer sees the
   stale refined type.

**Known, still-open residual gap**: an *immutable* pointer taken before a
later `&mut` of the same binding isn't retroactively invalidated —
`ptr := &a; mut_ptr := &mut a;` leaves `ptr` holding a possibly-now-stale
refined type. Closing this needs real aliasing/borrow-checking, which is
out of scope; this is a known, documented gap rather than an oversight.

## Compound assignment & increment/decrement

`+= -= *= /= %= &= |= ^= <<= >>=` and `++`/`--` all desugar entirely during
analysis (never reach codegen as their own node) — they resolve the target
place once, then reuse the ordinary binary-op/assignment machinery. Both
require the target to be a genuinely mutable place, checked through the
same `require_mutable_place` helper every other write-position uses
(assignment, `&mut`, a `mut self` call's implicit auto-ref).
