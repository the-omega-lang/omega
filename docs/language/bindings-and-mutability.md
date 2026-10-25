# Bindings and mutability

This chapter is normative for current Omega language behavior. Known implementation limitations are tracked under [`../issues/`](../issues/).

## Binding forms

```omega
a : i32;
a : i32 = 10;
a := 10;
^a : i32 = 10;
^a := 10;
```

A binding is immutable unless marked `^`, written directly before its name; in a `comp` binding the `^` follows `comp` (`comp ^x`), which [`compile-time-evaluation.md`](compile-time-evaluation.md) rejects. `:=` infers the binding's type from its initializer; `:` supplies an explicit type. An initialized binding's annotation may leave parts to its initializer with `_` holes (`a : Pair<_, u8> = ...`, `a : [_]u8 = [1, 2, 3]`), and `a : _ = 10` is exactly `a := 10`; see [`generics.md`](generics.md#inference-holes). Top-level inferred bindings have additional compile-time requirements described in [`compile-time-evaluation.md`](compile-time-evaluation.md).

`mut` has no special meaning; it is an ordinary identifier. Why mutability is a symbol rather than a keyword is recorded in [`syntax-principles.md`](syntax-principles.md).

A module-level binding owns storage with a linker symbol, so it accepts `@symbol` to name that symbol and decide its binary visibility; see [`annotations-and-sizeof.md`](annotations-and-sizeof.md#symbol). A local binding has no such symbol and accepts no annotation.

Every top-level binding, including a `comp` one, also accepts `@cond` to select whether it is declared at all; a local binding does not, because a condition selects whole declarations rather than statements.

## Shadowing

A local declaration always introduces a **fresh binding**. It never writes to an
existing one, so a declaration may reuse a name already bound in the same block
or in an enclosing scope:

```omega
^x := read_privileged();
x := x.narrow();      # fresh immutable binding; the initializer reads the old one
```

The rules are:

- **Initializer first.** A declaration's initializer is analyzed in the
  environment that exists immediately *before* the declaration, so `x := x`
  reads the previous binding. The new binding becomes visible from the
  declaration onward; an uninitialized declaration (`x : i32;`) shadows from
  that point too.
- **Fresh identity.** The new binding has its own type, mutability, and storage,
  each subject to its ordinary rules. Shadowing may drop `^`, add `^`, or
  change the type. The shadowed binding keeps its own identity and is unaffected.
- **No escape within a block.** Once a name is shadowed in the same block, later
  source in that block cannot name the older binding again.
- **Inner scopes restore.** A binding introduced in a nested block disappears
  when that block ends, revealing the outer binding again.
- **Locals win over module names.** A local binding hides a module-scope
  declaration, function, or imported value of the same spelling for value and
  call lookup. Shadowing does not merge namespaces: a local value binding does
  not replace a type spelling.
- **Ordinary cost.** Shadowing introduces no move, freeze, or storage-reuse
  semantics. `x := x` is an ordinary initialization from the previous binding
  and costs exactly what that initialization costs.

Declaration sets that require unique names are unaffected: two parameters of the
same function, two fields of a struct, or two module-scope declarations of the
same name remain errors. Macro hygiene is also unaffected: a name authored by a
macro body and a caller-authored name with the same spelling are different
bindings, and neither shadows the other.

Local `comp` bindings shadow under the same rules; see
[`compile-time-evaluation.md`](compile-time-evaluation.md).

## What may be mutable

Mutability is expressed independently for:

- local and global bindings and `for`-in bindings (`^x`),
- function parameters (`^a : A`), each a mutable local of the function body (see [`functions.md`](functions.md#mutable-parameters)),
- pointer pointees (`^T` versus `*T`), and
- method receivers' pointees (`^self` versus `*self`).

A parameter without `^` and every aggregate field are immutable bindings; a field's value can still be mutated through an appropriately mutable place/pointer.

A `foreign ^name : Type;` binding makes an external data symbol mutable; without `^`, Omega code cannot write through that binding. Foreign function symbols cannot be declared `^`. Absence of `^` does not imply that external data is constant; see [`foreign-function-interface.md`](foreign-function-interface.md#foreign-bindings).

## Receiver forms

A method receiver has one of three forms:

```omega
self    # by-value, immutable local copy
*self   # self : *Self, immutable pointee
^self   # self : ^Self, mutable pointee
```

The pointer receivers are spelled like the address a call site takes (`*value`, `^value`), so `^self` means `self : ^Self`. It is the one place where `^` before a parameter name does not declare a mutable binding. There is no by-value mutable receiver: a method that needs a mutable copy of its receiver declares one in its body (`^copy := self;`).

Calling a pointer-receiver method on a value automatically takes the required reference when legal. Calling a by-value receiver on a pointer dereferences and copies the value when legal.

`^self` is the receiver form for mutating the caller's pointee.

A spec method cannot use a by-value `self` receiver; spec receivers must be pointer-shaped so dynamic dispatch does not require the erased concrete value size. See [`specs-and-conformance.md`](specs-and-conformance.md).

## Mutable places

Assignment, compound assignment, increment/decrement, `^place`, and calls requiring a mutable receiver require a mutable place. A merely mutable *binding that contains an immutable pointer* does not make the pointee mutable, and an immutable binding cannot be written merely because its value type is mutable elsewhere.

`*place` produces an immutable pointer (`*T`). `^place` requires a mutable place and produces a mutable pointer (`^T`); each address-taking expression is spelled like the type it produces. A dereference `p.*` is itself a place, and it is mutable exactly when `p` is a `^T`: writability comes from the pointer's type, never from the spelling of the access, so a write through a pointer is always `p.* = value`.

When enum-variant refinement has narrowed a binding, taking `^` of that binding widens the binding and pointer pointee back to the enclosing enum type, because a mutable alias could replace the current variant. Immutable pointers may preserve safe refinement. A remaining aliasing limitation is tracked in [`../issues/language-limitations.md`](../issues/language-limitations.md).

An anonymous enum narrowed to one of its members behaves the same way. Reading the binding, accessing a field, indexing, or calling a method uses the member's type, while the binding's storage remains the whole anonymous enum, and taking `^` of it widens back to the anonymous enum. As with a named enum, a narrowed binding is not itself an assignment target while the proof holds; it is one again outside the arm. Refinement is proof about the current value, never a change of representation.

A `match` arm with field patterns also refines **field paths** below the binding — `res.value` proved to be `Result::Ok`, for example. The same rules apply at every refined path:

- Reading through it uses the refined type, so `res.value.value` is available inside the arm.
- Taking `^` of a refined path, or of any prefix of it, or calling a method that needs a mutable receiver on one, gives up every refinement at and below that place for the rest of the arm. `^res` gives up all of them.
- A refined path, and every prefix of it down to the binding itself, is not an assignment target while the proof holds: the write would replace the value the arm relies on. Assigning *inside* a refined field — `res.value.value = 3` — leaves the proof intact and is allowed.
- `*` of a refined path produces a pointer to the field's declared type, since the pointer may outlive the arm that proved more.

## Compound assignment and increment/decrement

The compound assignments are:

```text
+= -= *= /= %= &= |= ^= <<= >>=
```

`x op= y` has the same operator semantics as computing `x op y` and storing the result back into `x`, while evaluating the target place only once.

`++` and `--` update a mutable numeric place by one. They are statements/update expressions and require the same mutability checks as assignment.
