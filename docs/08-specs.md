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

A *generic* spec may forward its own still-abstract generics into a
dependency's type arguments (`spec Labeled<T> : Container<T>`) — resolved
lazily, alongside `Self`, once a concrete implementor is actually being
checked, mirroring exactly how the spec's own functions are resolved.
Identifying *which* spec a dependency names is still eager (needed for
dynamic-dispatch vtable slot ordering, which has no resolver of its own to
defer to) via a dedicated, args-independent spec lookup — see
[generics](06-generics.md) for the full mechanism.

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

### Implementing the same generic spec more than once

```
spec Consumer<T> {
    consume(*self, value: T) => i32;
}

struct Multi : Consumer<i32>, Consumer<*u8> {
    exposed consume(*self, value: i32) => i32 { value + 1 }
    exposed consume(*self, value: *u8) => i32 { puts(value) }
}
```

Supported: a type may implement the same generic spec at different type
arguments, satisfied by ordinary overloading. Every requirement — across
every `implements`-clause entry, not just within one — is matched against
the implementor's own methods by **exact `(name, signature)`**, never by
name alone: `Consumer<i32>`'s `consume(*self, value: i32)` and
`Consumer<*u8>`'s `consume(*self, value: *u8)` are two independent
requirements, each satisfied by its own overload, the same way any other
overloaded method already works here. This was previously a hard
`ConflictingSpecFunctions` error, purely because the matching was
name-only — not a deliberate restriction, a compiler bug. Fixed at the
root: the same name-only matching also existed one level deeper, in the
dynamic-dispatch vtable builder (it resolved each vtable slot by matching
a concrete method's *name* alone, which stopped being enough to pick the
right one the moment two same-named overloads could both be pointed at by
the same spec's own flattening) -- see `Analyzer::type_implements_spec`'s
own doc comment for how each vtable slot's concrete method is now
precomputed once, during analysis, instead of re-derived from a bare name
in codegen.

`MissingSpecFunction` also names *which* instantiation is missing
(`Consumer<*u8>`, not just `Consumer`) once more than one is in play —
`FlattenedSpecFn::type_args` (derived from the same `substitution` a
queued default body's own phase-2 check already relies on) feeds a small
shared `generic_name` diagnostic helper (`error/mod.rs`), since
`ResolvedType::Spec`'s own `Display` deliberately stays bare (its
`type_args` exist for mangling, not diagnostics).

This is still bounded by ordinary overload rules, not a new exception to
them: **overloading here is parameter-type-only** — a spec function
shaped like `get(*self) => T`, varying *only* in return type across
instantiations, can never be satisfied twice (`get(*self) => i32` and
`get(*self) => *u8` collide as an outright `Redeclaration`, the identical
rule that already blocks return-type-only overloading anywhere else in
this language — see `check_overload_duplicates`). Implementing the same
generic spec twice only actually works when the varying type shows up in
a *parameter*, not only in the return type.

Dynamic dispatch (`spec *Consumer<i32>` vs. `spec *Consumer<*u8>`) works
identically for a doubly-implemented type — each instantiation gets its
own, independently-correct vtable. See "Dynamic dispatch" below for why
that needed its own fix, not just the analyzer-side one above.

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
one new piece: a static data blob per resolved vtable-slot list (in
practice, per distinct `(concrete type, spec, spec type args)` coercion —
see the caveat below for why the cache key isn't literally that triple),
built once and cached, with a function-pointer relocation per vtable slot (the
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

### Casting into a spec object

```
obj := <spec *Animal>&dog;
```

An explicit `<spec *Spec<Args>>expr` cast, independent of (and not
subject to) the 4-site limitation above — this simply didn't exist at all
until it was added for [for-in loops](18-for-in-loops.md)'s explicit
disambiguation syntax, and generalizes cleanly beyond that one use case.
Unlike every numeric/pointer cast, this one can't be decided by a pure
width/signedness computation — it has to *prove* something (`expr`'s
pointee genuinely implements `Spec<Args>`), so `Analyzer::analyze_cast`
checks it first, as its own family, before ever reaching the ordinary
`cast_class`-based machinery (mirroring how the `*str`/`*[u8]`/`*[i8]`
byte-pointer family already gets its own first-checked special case, for
the identical reason — neither fits the scalar-width model at all). Runs
the exact same proof `coerce_to_expected` runs for the implicit version of
this coercion (`Analyzer::type_implements_spec`), including the identical
mutable-pointer-needs-a-mutable-source rule ordinary pointer casts already
enforce.

## `spec T` — static dispatch as a type

```
my_function(thing: spec SomeSpec) => void { ... }
```

`spec SomeSpec` (no `*`) is Rust's `impl Trait` — a *static*-dispatch spec
bound written directly as a type, contrasted with `spec *SomeSpec` above
(a genuine dynamic-dispatch fat pointer). It has three positions, each
with different mechanics under the hood, though all three share the same
"some concrete type satisfying this bound" reading:

### Parameter position

Pure sugar, desugared away entirely during HIR lowering — before semantic
analysis ever runs, `my_function(thing: spec SomeSpec) => void` becomes:

```
my_function<T: SomeSpec>(thing: T) => void { ... }
```

an ordinary bound generic parameter (a fresh, compiler-minted name), so
every existing generic-bound mechanism — argument-driven inference,
`ensure_item`'s bound-checking, monomorphization — applies completely
unmodified. Each occurrence gets its own independent generic (two `spec
SomeSpec` parameters in one function are never required to share a
concrete type, matching Rust: `f(a: impl Foo, b: impl Foo)`), and this
recurses through compound shapes (`thing: *spec SomeSpec`, the common
"pass by pointer" idiom this language already uses for explicit bound
generics — e.g. `animal: *T` above) the same way generic-argument
unification already does elsewhere.

### Return position, inside a spec's own function declaration

```
exposed spec ToIterator<T> {
    to_iterator(*self) => spec Iterator<T>;
}
```

This is the associated-type-like case: rather than every implementor's
`to_iterator` needing to return the *exact same* concrete type (impossible
here — each implementor's iterator is its own type), each implementor's
own concrete return type is checked against the `Iterator<T>` bound
(`Analyzer::type_implements_spec`) instead of matched by exact-signature
equality the way every other spec requirement still is. The requirement
itself carries no concrete return type at all — see
`FlattenedSpecFn::return_type_bound`.

**This makes the spec no longer object-safe.** `spec *ToIterator<T>`
doesn't exist — no single vtable slot can represent "whichever concrete
type each implementor happens to return," the identical reason Rust's own
`IntoIterator` isn't object-safe. Attempting one produces a dedicated
`SpecNotObjectSafe` diagnostic rather than a malformed vtable.
`ResolvedSpecType::is_object_safe` is computed once, eagerly, the moment a
spec's own signature is resolved (`false` the instant any of its own
functions — or any dependency's — has a `spec T` return requirement), and
checked at the one place a `spec *T` type actually gets built.

### Return position, on an ordinary (non-spec) function

```
make_dog() => spec Animal {
    Dog {}
}
```

The concrete return type is *inferred from the function's own body* —
every `return`/tail exit point must resolve to the exact same concrete
type (Rust's `impl Trait` rule: one concrete type, not merely "each
individually satisfies the bound"), which must itself implement the
declared bound. This is the most involved of the three: it inverts the
compiler's ordinary signature-before-body ordering (`collect_function_
signature` has no concrete type to give `resolve_type_or_error` for a
`spec T` return type at all), so the driver eagerly body-checks such a
function — twice: a throwaway probe pass to discover the concrete type
(`Analyzer::infer_body_return_type`, `expected = None` throughout, its own
diagnostics discarded on success), then the ordinary, unmodified
`check_function_body` once the type is known, which is what's actually
cached and used everywhere (`Driver::resolve_spec_return_function`).
Diverges to `AmbiguousSpecReturnType` (two different concrete types
across exit points), `SpecReturnTypeUnconstrained` (no exit point to infer
from at all), or an ordinary bound-violation diagnostic, as appropriate.

Two functions whose inference calls each other would otherwise recurse
forever (neither one's *own* signature key is ever `InProgress` for
itself the way ordinary same-key recursion is caught) — guarded by a
dedicated stack (`SpecReturnTypeRecursion`), independent of (and more
general than) the ordinary single-key cycle guard. In practice, ordinary
call resolution's own `InProgress` tracking already catches this first
(as a generic cyclic-dependency error) for any recursion reachable through
a normal function call, since callee-signature resolution for *any*
function funnels through the same query the dedicated guard backstops.

Not yet supported for struct/enum/union **methods** or overloaded free
functions — both go through a different, harder-to-retrofit signature-
collection path (`compute_aggregate`/`collect_methods`); a `spec T` return
type there is rejected with the same `SpecStaticNotAllowedHere` diagnostic
any other unsupported position gets.

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

- **A vtable's real cache/dedup key is its own resolved slot list
  (`Analyzer::type_implements_spec`'s output, one concrete method's
  `decl_id` per slot), not `(concrete type, spec, spec type args)`
  directly.** The two coincide almost always, but the slot list is
  strictly more precise: two coercions that happen to resolve to the
  identical ordered method list always produce byte-identical vtables no
  matter which concrete type or spec they came from, so sharing one copy
  is correct even then. The *symbol name* still has to be a function of
  `(concrete, spec, spec type args)` though (`decl_id`s aren't meaningful
  across separately-compiled translation units) — see
  `mangle::vtable_symbol`.
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
