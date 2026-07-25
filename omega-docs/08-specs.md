# Specs

Omega's interface/trait system: function-only contracts, static dispatch
through generic bounds, dynamic dispatch through fat trait-object pointers,
and — separately — a way to attach methods to primitives.

## Declaration, dependencies, defaults

```
exposed spec Animal {
    kind(*self) => AnimalKind;                     # required
    make_sound(*self) => *u8;                       # required
    is_dog(*self) => bool {                            # default
        self.kind().tag == AnimalKind::Dog.tag
    }
    same_kind(*self, other: *Self) => bool { ... }        # default, uses Self
}

exposed spec Mammal : Animal, Dummy {         # depends on two other specs
    dummy(*self) => void { ... }               # satisfies Dummy's own requirement
}

spec MySpec = Dummy | Mammal;                    # pure alias — no functions of its own
```

A function with a body is a *default*, used as-is unless a concrete
implementor overrides it. `Self` inside a default body means "whatever
concrete type is implementing this spec" — a `*Self` parameter becomes
`*Dog` for `Dog`'s own instantiation, `*Cat` for `Cat`'s, and so on. A spec
may itself depend on other specs (`spec Mammal : Animal, Dummy`); an
implementor of `Mammal` must satisfy `Mammal`'s own requirements *plus*
`Animal`'s and `Dummy`'s, and `Mammal`'s own defaults may freely call
either dependency's functions on `self`. A spec can satisfy one of its own
dependency's bare requirements with a default of its own — an implementor
never has to provide it directly in that case.

Same name + identical resolved signature reached through two different
paths (e.g. two dependencies both requiring it) → silently deduplicated
into one requirement. Same name + a genuinely different signature →
`ConflictingSpecFunctions`.

**Spec functions always receive `self` by pointer** (`*self`/`*mut self`)
— by-value self is rejected at the spec's own definition
(`SpecSelfMustBePointer`), since dynamic dispatch erases the concrete type
to a bare data pointer with no size information to copy a value from.

## Implementing

```
struct Dog : Animal {
    exposed id: i32;
    exposed kind(*self) => AnimalKind { AnimalKind::Dog }
    exposed make_sound(*self) => *u8 { <*u8>b"woof woof\0" }
}
```

`struct/enum/union : Spec1, Spec2 { ... }` — every required function
without a matching own-signature method, that has no default either, is
`MissingSpecFunction`. An implementor's satisfying method must also meet
the spec's own visibility floor — see
[visibility](07-visibility.md)'s inheritance + minimum-permissiveness rule.

## Static dispatch (generic bounds)

```
make_sound_with_static_dispatch<T: Animal>(animal: *T) => void {
    puts(animal.make_sound());
}
```

Because Omega's generics fully monomorphize (see [generics](06-generics.md)),
static dispatch needed **zero new codegen** — once a concrete type's method
list is fully populated with every spec-required method (own override or
spec-default instantiation), `animal.make_sound()` inside the bound generic
body resolves through the exact same `find_methods` lookup any ordinary
method call already uses. All of static dispatch reduces to (1) correctly
populating that list and (2) a bound-satisfaction check for a better error
than a bare "no such method." Nominal, not structural — `T: Animal`
requires a real `: Animal` declaration; an unbound generic parameter still
works by pure duck-typing as before.

`T: SpecAlias` (`accepts_myspec<T: MySpec>`) requires everything every
spec the alias expands to demands, all at once.

## Dynamic dispatch

```
make_sound_with_dynamic_dispatch(animal: spec *Animal) => void {
    puts(animal.make_sound());
}
# call site:
make_sound_with_dynamic_dispatch(&dog);        # &Dog coerces to spec *Animal
```

`spec *Animal` is a genuine fat pointer — `[data pointer, vtable pointer]`,
the same 2-leaf template `*[T]` slices already established. Every Omega
call already compiles to `call_indirect` (there is no direct-call
instruction anywhere in this backend), so the vtable mechanism only needed
one new piece: a static data blob per `(concrete type, spec)` pair, built
once and cached, with a function-pointer relocation per vtable slot (the
exact same relocation mechanism const-slice rodata already used for
pointer-shaped elements). `CheckedExpr::SpecCoerce` marks the one coercion
in the whole type system that genuinely changes representation (a 1-leaf
pointer becoming a 2-leaf fat pointer) — every other implicit coercion
(e.g. refined-enum widening) is representation-preserving and needs no
explicit node at all.

Coercion into `spec *T` happens at 4 sites: ordinary call arguments,
assignment, declaration-with-init, and `return` — **not** yet struct-
literal fields, array-literal elements, or a bare tail-return without the
`return` keyword (a documented, narrow gap).

## `for`-attached specs: giving primitives methods

```
spec StrOps : Eq for str {
    equals(*self, other: Self) => bool { ... }
}
spec SliceImpl<T> for [T] {
    first(*self) => T { self[0] }
}
```

`spec Name : Deps for Target { ... }` both defines and immediately,
anonymously implements a spec for a primitive `Target` in one statement —
this is the *only* way to give a scalar/`str`/slice type its own methods.
`Name` is never registered anywhere (two unrelated `for` blocks may reuse
the same name with zero conflict) — the identity that matters is
`(spec, target)`, and **exactly one `for` block per target type is allowed,
enforced globally**, which is what eliminates any cross-spec merge/conflict
question for a receiver entirely (a receiver matches at most one spec by
construction).

Restricted to **`core` only** (Omega's standard-library module tree — see
[core library](13-core-library.md)), and only three target shapes: the
built-in scalar/`bool`/`char`/`void` set, `str`, or the pattern shape `[T]`
(a spec's own single generic parameter, referencing a slice of it). This
replaced an earlier, explicitly rolled-back `@ufcs` annotation design — the
user judged the annotation approach as fighting the language's own syntax;
reusing ordinary `spec` grammar for the same purpose was simpler and more
consistent.

Extension methods are discovered **lazily**, the first time any primitive
method lookup needs them, by walking `core`'s own `import` graph
transitively from its root (reusing the same worklist the compiler's
ordinary reachability sweep is built from) — not a filesystem directory
walk. There is no ambient-import mechanism for these methods at all (unlike
the rejected `@ufcs` design): since the spec name is unregisterable, calls
resolve purely through the ordinary method-lookup fallback, independent of
what's imported.

## Caveats

- **A spec's own generics can't be forwarded into a dependency's type
  args** (`spec Foo<T> : Bar<T>` — `T` reports unresolved; `spec Foo<T> :
  Bar<i32>`, a concrete argument, is fine). Traced precisely: a
  dependency's type args are resolved eagerly, at the depending spec's
  own one-time, argument-free declaration pass — before `T` is ever bound
  anywhere (unlike the spec's own *functions*, which correctly stay raw
  until a concrete implementor's `Self` + generics are known via
  `flatten_spec_into`). Making dependencies lazy the same way needs specs
  to get their own args-independent cell identity (cached by `(module,
  name)` alone, not `(module, name, type_args)`) — real, buildable
  (mirrors the existing `generic_function_signature` precedent for "this
  item kind doesn't fit the ordinary args-bound lookup"), but it touches
  the shared module-resolver trait and every existing spec-reference call
  site, so it's left as a scoped, understood, deferred gap rather than a
  rushed change to resolution infrastructure every generic item kind
  relies on. See [generics](06-generics.md) for the equivalent write-up.
- **Spec implementation is struct/enum/union only** for ordinary specs — no
  primitives outside the dedicated `for`-attachment mechanism above.
- **No `is_variadic` support** on spec functions.
- **Coercion into `spec *T` isn't wired into every expression position** —
  see the 4-site list above.
- A fully degenerate program that never calls *any* `for`-attached method
  anywhere never triggers extension discovery at all, so a malformed
  `for`-spec inside `core` goes unvalidated in that one case — consistent
  with this compiler's general "only what's referenced gets analyzed"
  philosophy, not a regression specific to this feature.
- See [visibility](07-visibility.md) for the dynamic-dispatch visibility
  gap this system's `Private`-method owner-scoping opened and how it was
  closed.
