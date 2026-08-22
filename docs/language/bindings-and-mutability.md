# Bindings and mutability

This chapter is normative for current Omega language behavior. Known implementation limitations are tracked under [`../issues/`](../issues/).

## Binding forms

```omega
a : i32;
a : i32 = 10;
a := 10;
mut a : i32 = 10;
mut a := 10;
```

A binding is immutable unless marked `mut`. `:=` infers the binding's type from its initializer; `:` supplies an explicit type. Top-level inferred bindings have additional compile-time requirements described in [`compile-time-evaluation.md`](compile-time-evaluation.md).

`mut` is contextual syntax. It does not make `mut` globally unavailable as an identifier outside positions where the grammar recognizes a mutability modifier.

## What may be mutable

Mutability is expressed independently for:

- local and global bindings (`mut x`),
- pointer pointees (`*mut T` versus `*T`), and
- method receivers (`mut self` and `*mut self`).

Ordinary function parameters and aggregate fields are immutable bindings; a field's value can still be mutated through an appropriately mutable place/pointer.

A `foreign` binding is an immutable symbol binding.

## Receiver forms

A method receiver has one of four forms:

```omega
self       # by-value, immutable local copy
mut self   # by-value, mutable local copy
*self      # pointer receiver, immutable pointee
*mut self  # pointer receiver, mutable pointee
```

Calling a pointer-receiver method on a value automatically takes the required reference when legal. Calling a by-value receiver on a pointer dereferences and copies the value when legal.

`mut self` mutates only the method's local by-value copy; it does not mutate the caller's value. `*mut self` is the receiver form for mutating the caller's pointee.

A spec method cannot use a by-value `self` receiver; spec receivers must be pointer-shaped so dynamic dispatch does not require the erased concrete value size. See [`specs-and-conformance.md`](specs-and-conformance.md).

## Mutable places

Assignment, compound assignment, increment/decrement, `&mut`, and calls requiring a mutable receiver require a mutable place. A merely mutable *binding that contains an immutable pointer* does not make the pointee mutable, and an immutable binding cannot be written merely because its value type is mutable elsewhere.

`&place` produces an immutable pointer. `&mut place` requires a mutable place and produces a mutable pointer.

When enum-variant refinement has narrowed a binding, taking `&mut` to that binding widens the binding and pointer pointee back to the enclosing enum type, because a mutable alias could replace the current variant. Immutable pointers may preserve safe refinement. A remaining aliasing limitation is tracked in [`../issues/language-limitations.md`](../issues/language-limitations.md).

## Compound assignment and increment/decrement

The compound assignments are:

```text
+= -= *= /= %= &= |= ^= <<= >>=
```

`x op= y` has the same operator semantics as computing `x op y` and storing the result back into `x`, while evaluating the target place only once.

`++` and `--` update a mutable numeric place by one. They are statements/update expressions and require the same mutability checks as assignment.
