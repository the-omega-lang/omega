# Syntax principles

**Non-normative rationale.** This chapter explains why Omega's pointer and mutability syntax is spelled the way it is. It defines nothing: the rules themselves are in [`grammar.md`](grammar.md), [`bindings-and-mutability.md`](bindings-and-mutability.md), [`functions.md`](functions.md), and [`control-flow-and-operators.md`](control-flow-and-operators.md), and those chapters win wherever this one seems to disagree.

The spellings it explains:

| Meaning | Spelling |
|---|---|
| mutable binding | `^x := 1;`, `^x : T;` |
| mutable parameter | `f(^a : A)` |
| read-only / mutable pointer type | `*T` / `^T` |
| take a read-only / mutable address | `*place` / `^place` |
| dereference, for reading or writing | `p.*`, `p.* = v`, `p.*.field` |
| receivers | `self`, `*self`, `^self` |

## 1. Symbols over keywords

Reserved syntax is spelled with symbols, and words are left to be names. Mutability is a property every reader has to see at a glance, so it gets a symbol (`^`) rather than a keyword. A keyword such as `mut` would either be reserved everywhere, taking a word away from every program, or be contextual, which makes the same word a name in one position and syntax in the next. With `^`, `mut` is an ordinary identifier.

## 2. One symbol, one meaning

Within the pointer and mutability syntax each symbol has exactly one meaning. `^` always means *mutable*: a mutable binding, a mutable parameter, a mutable pointer type, or the mutable address of a place. `*` always means *read-only pointer*: a pointer type or the read-only address of a place. Neither symbol also means "dereference", and there is no keyword doing part of the same job.

## 3. Expressions are spelled like their types

An address-taking expression is spelled like the type it produces: `*x` has type `*T`, and `^x` has type `^T`. Reading `p := ^counter;` tells you that `p` is a `^Counter` without looking anything up.

C's rule runs the other way: a declaration mirrors its use, so `int *p` declares the thing that `*p` produces. That rule fits C because C writes types around the declared name. Omega writes every type as a prefix that stands on its own (`*T`, `[4]T`, `(A) => R`), and a prefix type has no use to mirror. Matching the expression to the type is the useful correspondence here.

## 4. Prefix builds, postfix reaches in

Prefix operators build a new value from a place: `*x` and `^x` produce pointers to it. Postfix steps reach into a value: `.field`, `[i]`, `(args)`, and `.*` each take the value on their left and go one step further in. A path through pointers therefore reads strictly left to right: `node.next.*.value` is "take `node`, take its `next` field, follow that pointer, take its `value`".

C needed `->` because its dereference is prefix: `*p.x` means `*(p.x)`, so reaching a field through a pointer is `(*p).x`, and `->` is the shorthand for that one case. With a postfix dereference, `p.*.x` already reads in order and needs no extra operator.

## 5. Precedence follows from it

Postfix steps bind tighter than prefix operators, so `*a.b.c` is `*(a.b.c)`: the address of the place the whole path names. That is the reading a prefix "builder" should have, since the thing it builds from is the complete place to its right. `^a[i]` is likewise the mutable address of `a[i]`.

## 6. A dot means a place

Every postfix step that starts with a dot names a place: `.field` names part of a value, and `.*` names the pointee. They chain the same way and can be assigned to, taken the address of, and read. Bare postfix `?` is deliberately not spelled with a dot, because it is control flow (it may return from the enclosing function), not a step to another place.

## 7. Position decides meaning

A symbol's role comes from where it appears, not from a second spelling. `*` and `^` are prefix operators before an operand and binary multiplication and xor between two operands. In the same way, whether a dereference may be written comes from where it appears and from the pointer's type, not from how the dereference is spelled: `p.*` is read on the right of `=` and written on the left, and it may be written only when `p` is a `^T`. That is why the dereference is always `.*`. A separate mutable dereference such as `.^` would claim, at the access, a mutability that only the pointer's type can grant.

## The receiver exception

Receivers mirror the address a call site takes. Calling `value.bump()` on a `^self` method passes `^value`, and calling a `*self` method passes `*value`, so `*self` is `self : *Self` and `^self` is `self : ^Self`. This is the one place where `^` before a name does not declare a mutable binding: in a receiver it names the pointer the call site builds.

Omega has no by-value mutable receiver. Spelling one would need a third form beside `*self` and `^self`, and `^self` is already the natural spelling of the much more common mutable-pointer receiver. A method that wants a mutable copy of its receiver says so in its body with `^copy := self;`, which costs exactly what the receiver form would have cost and keeps receiver syntax to the three forms a call site can produce.
