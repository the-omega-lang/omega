# Specs

Omega's interface/trait system: function-only contracts, static dispatch
through generic bounds, dynamic dispatch through fat trait-object pointers,
explicit conformance, and a core-only way to attach inherent methods to
primitive types.

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
struct Dog {
    exposed id: i32;
}

conform Dog to Animal {
    kind(*self) => AnimalKind { AnimalKind::Dog }
    make_sound(*self) => *str { "woof woof" }
}
```

`conform Target to Spec { ... }` is the only conformance declaration. The
block must contain exactly the spec requirements it implements: a missing
required function is `MissingSpecFunction`, and an extra function is
`ConformanceExtraFunction`. Inherent methods never satisfy a requirement; only a
body in this conform (or a spec default) does. A conform method has no
visibility modifier; it inherits the requirement's visibility.

Conformance is nominal. A concrete declaration is legal when either the target
type or the spec belongs to the current package. A second equally-specific
conform for the same `(target, spec, spec arguments)` is rejected.

### Blanket conformances

```
conform<T: Numeric> T to Sum {
    sum(*self) => i32 { 0 }
}
```

This applies to every conformable `T` that satisfies `Numeric`; its body is
monomorphized only when such a `T` is used through `Sum`. A concrete conform
always wins over a matching blanket. Between two blankets, a bound with a
transitive dependency on the other bound is more specific (`Ord : Eq` beats
`Eq`); unrelated matching bounds are an `AmbiguousConformance` error rather
than an arbitrary declaration-order choice. A blanket may also be written
*unbounded* (`conform<T> T to Spec`), which accepts every conformable type and
is therefore strictly less specific than any bounded blanket for the same spec.

Precedence is decided at registration, so exactly one declaration ever owns a
given `(target, spec, spec arguments)` and only the winner's body is emitted.
That includes dependency stand-ins: `conform Foo to Derived` supplies `Foo`'s
`Base` conformance, and being specific to `Foo` it beats a blanket that matched
`Foo` only incidentally — regardless of which was registered first.

A blanket may implement only a spec declared by its own package. Since its
target can be a foreign type, allowing a foreign spec would defeat the orphan
rule for every downstream package (`BlanketConformanceForeignSpec`).

The target may be a named type, `str`, a primitive scalar, or a slice. A
pointer, inline array, function, or spec-object target is rejected with
`ConformTargetNotAType`. Conforming to a dependent spec registers conformance for its
dependencies too (`conform Foo to Derived` supplies `Base`'s requirements as
well); a spec *alias* as the conformed-to spec (`conform Foo to AB`, where `spec
AB = A | B`) works, but a `T: AB` bound is **not** satisfied by conforming
`A` and `B` separately. Slice targets (`conform []u8 to Eq`, `conform<T>
[]T to Eq`) parse and register but no call can reach them. See
[known-issues.md](14-known-issues.md)'s conformance section for all three.

Conforming instance methods do not become globally callable as ordinary
inherent methods. They are available through a generic bound (`T: Animal`),
or explicitly as `Animal::make_sound(&dog)`. A conforming static function is
called as `Target::function(...)`; two conformances providing the same static
call are diagnosed as ambiguous.

### Receiver adaptation in a spec-qualified call

`Spec::function(receiver, args...)` adapts `receiver` to the function's
declared self-mode with exactly the same rule `receiver.function(args...)`
uses (`Analyzer::adapt_self_argument`), so all four of these are legal and
mean the same thing:

```
Speak::speak(&dog)      # already a pointer -- reused, re-stamped
Speak::speak(dog)       # a place -- address taken
Speak::speak(p)         # p : *Dog -- reused
Speak::speak(make())    # an rvalue -- see the cost note below
```

The adaptation is **type-directed, not a uniform `&`**. Given `fmt(*self,
out: spec *mut Write)`, what `*self` actually means depends on what `Self` is:

| receiver | `*self` resolves to | what the call passes |
|---|---|---|
| `n : i32` | `*i32` | `&n` |
| `"hi"` (`*str`) | `Str` | the fat pointer itself, **no `&`** |
| `&buf[0..]` (`*[]u8`) | `Slice` | the fat pointer itself, **no `&`** |
| `p : *i32` | `*i32` | `p` |

`str` and `[?]T` *are* their own pointer representation, so `Self`
substitution re-stamps `*self` rather than wrapping it (see
`Context::resolve_pointer_type`) — writing `&"hi"` by hand would produce a
pointer to a pointer. This is precisely why `std::io`'s print macros can
spell `Display::fmt($args, &mut omega_print_out)` at all: a macro cannot know
which of these shapes its argument is, so the adaptation has to happen in the
compiler rather than in the macro body.

**Cost.** A receiver that is not a place — a literal, an arithmetic
expression, a call result — is materialized into a stack temporary whose
address is then taken. `Display::fmt(42, &mut w)` compiles to "store 42 into a
slot, pass the slot's address," not to anything cheaper. This is the one place
in a spec-qualified call where the source text does not show the whole cost.
It is not specific to `conform`: a receiver-position call on an rvalue
(`(1 + 2).fmt(&mut w)`) has always done the same thing, through the same code
path.

A `*mut self` requirement against an rvalue receiver is rejected rather than
silently writing into the discarded temporary — see
[known-issues.md](14-known-issues.md) for the diagnostic's current wording.

### Implementing the same generic spec more than once

```
spec Consumer<T> {
    consume(*self, value: T) => i32;
}

struct Multi {}

conform Multi to Consumer<i32> {
    consume(*self, value: i32) => i32 { value + 1 }
}
conform Multi to Consumer<*u8> {
    consume(*self, value: *u8) => i32 { puts(value) }
}
```

Supported: a type may implement the same generic spec at different type
arguments, with a separate conform block for each instantiation. Every
requirement is matched against
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
static dispatch uses the conforming method selected for the concrete
instantiation. `animal.make_sound()` is in scope because `T: Animal`; the
same call on a concrete unbound `Dog` is intentionally rejected with
`MethodNotInScope`, and can be written `Animal::make_sound(animal)` instead.
Nominal, not structural — `T: Animal` requires a real conform declaration;
an unbound generic parameter still works by pure duck-typing as before.

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
the same 2-leaf template `*[]T` slices already established. Every Omega
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

Coercion into `spec *T` happens at ordinary call arguments, assignment,
declaration-with-init, explicit and tail `return`, struct-literal fields, and
array-literal elements.

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
`cast_class`-based machinery (mirroring how the `*str`/`*[]u8`/`*[]i8`
byte-pointer family already gets its own first-checked special case, for
the identical reason — neither fits the scalar-width model at all). Runs
the exact same proof `coerce_to_expected` runs for the implicit version of
this coercion (`Analyzer::type_implements_spec`), including the identical
mutable-pointer-needs-a-mutable-source rule ordinary pointer casts already
enforce.

### Reading the two leaves directly: `.ptr`/`.vtable`

```
obj : spec *Animal = &dog;
raw := obj.ptr;      # *u8 (or *mut u8, mirroring `obj`'s own mutability)
vt := obj.vtable;    # always *u8, immutable -- the vtable is read-only rodata
```

`.ptr`/`.vtable` read the fat pointer's own two leaves directly, exactly
like `.length`/`.size` already do for `*[]T`/`*str`'s `[data_ptr, len]`
leaves — not real fields (the concrete implementor is erased, so there's
nothing to look up by name), so they're recognized before the ordinary
struct-field paths would reject `spec *Spec` outright. `.ptr`'s own
pointee is always the opaque `u8` (there's no concrete type left to name
it as); its mutability mirrors the spec object's own (`spec *mut Animal`
gives a `*mut u8`, `spec *Animal` a `*u8`). `.vtable` is always an
immutable `*u8` regardless — the vtable itself is always compiler-
generated, content-deduplicated rodata (see "Dynamic dispatch" above),
never writable.

Two spec objects coerced from the same concrete type share the exact same
`.vtable` address (the dedup cache keys purely on the resolved slot list,
not on which coercion produced it — see the caveat in "Dynamic dispatch"
above), so `.vtable` equality is a sound "are these two objects backed by
the same concrete type" check without needing to know what that type is.
`.ptr` equality is the ordinary "same underlying instance" check, exactly
like comparing two plain pointers.

Neither field is reachable from inside a `comp` evaluation (see
[compile-time evaluation](19-compile-time-evaluation.md)) — dynamic
dispatch has no `ConstValue` shape at all, so a `spec *Spec`-typed value
can never appear as a comp-evaluable base in the first place.

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

Struct, enum, and union methods use the same body probe during method
signature collection — but the probe runs while the owning type's cell is
still being populated, so such a body can read `self`'s fields and cannot
call the type's other methods (see
[known-issues.md](14-known-issues.md)). Overloaded free functions remain
outside this rule.

## Primitive methods

```
primitive str {
    exposed is_empty(*self) => bool { self.length == 0 }
}
primitive<T> []T {
    exposed first(*self, out: *mut T) => bool { ... }
}
```

`primitive Target { ... }` defines inherent methods for compiler-provided
types that cannot contain their own declaration bodies. It is restricted to
the `core` package and to scalar, `bool`, `char`, `str`, and generic slice
targets; `void` is not a valid target. Exactly one primitive block may target
each concrete type. Functions carry ordinary visibility modifiers and are
called like inherent methods.

Primitive methods and spec conformance are deliberately separate. Only core
adds inherent primitive methods, but a package that owns a spec or its target
may add the corresponding conform block. Omega's standard-library primitive
conformances therefore live in `std::primitives`, for example `conform str to
Eq { equals(*self, other: Self) => bool { ... } }`. This keeps specs named and
independently conformable instead of inventing an anonymous interface as a side
effect of adding methods.

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
- **Only `core` can add inherent methods to primitives.** Any package allowed
  by the orphan rule can conform a concrete target to a spec.
- **A spec function may not be variadic.** `f(*self, ...)` is rejected at the
  spec's own declaration (`VariadicSpecFunctionUnsatisfiable`): Omega has no
  variadic function *definitions* — only `extern` declarations may be
  variadic — so no `conform` block or spec default could ever supply a
  matching body. The plumbing behind it is complete; the guard lifts when
  variadic definitions exist. See [known-issues.md](14-known-issues.md).
- **A `spec T` return type is not inferred on a method**, only on a plain
  top-level function — a method gets `SpecStaticNotAllowedHere`. A conform
  method satisfying a `=> spec Bound<...>` requirement declares its own
  *concrete* return type (`std::list`'s `to_iterator(*self) =>
  ListIterator<T>`), which is checked against the bound.
- Generic primitive and conform templates are instantiated lazily for the
  concrete target types a compilation uses.
